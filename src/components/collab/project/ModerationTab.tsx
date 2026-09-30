import { useCallback, useEffect, useRef, useState, type JSX } from 'react';
import { Loader2, X } from 'lucide-react';
import { api } from '../../../api';
import { Checkbox } from '../../settings/Checkbox';
import ProjectFrameTable, { type TableAction } from './table/ProjectFrameTable';
import { fromModeration, type FrameVM } from './frames';
import type { ModerationFrameView, ProjectFrameView } from '../../../types/models';

const REASON_MAX = 500;

/** The hub's stable 409 text for "this frame is no longer pending" — already
 *  approved/rejected by another moderator, or swept up by THIS SAME batch's
 *  own `trust: true` cascade (`approve_collab_frame` can retroactively
 *  publish every other pending frame from the same publisher in one hub
 *  call). Benign: the frame is decided either way, so the loop counts it and
 *  moves on instead of stopping. Stable core text from `decide_err` in
 *  `crates/athenaeum-core/src/api/collab.rs`; same string-match convention as
 *  `PUBLISH_BUSY` in `src/pages/ProjectDetail.tsx`. */
const ALREADY_DECIDED = 'This frame was already decided';

function isAlreadyDecided(msg: string): boolean {
  return msg.includes(ALREADY_DECIDED);
}

/** Keys the library (manifest mirror) by frame uuid, for `fromModeration`'s
 *  night/camera/metric fill-in when the frame has already landed. */
function byUuid(library: ProjectFrameView[] | null): ReadonlyMap<string, ProjectFrameView> {
  const map = new Map<string, ProjectFrameView>();
  for (const f of library ?? []) map.set(f.frameUuid, f);
  return map;
}

/**
 * Moderation tab — the coordinator's queue of pending first publications, as
 * one `ProjectFrameTable` (Task 12, spec 2026-09-30 "Moderation"). Replaces
 * `ModerationQueue`'s per-row list with batch Approve/Reject; `ModerationQueue`
 * itself is untouched here (Task 16 deletes it once the redesigned page is
 * wired up). The "Trust this publisher" checkbox moves from per-row (the old
 * queue) to one table-wide toggle in the toolbar, default on, applied to
 * every frame a batch Approve covers.
 */
export default function ModerationTab({
  projectId,
  library,
  onDecided,
  onOpen,
}: {
  projectId: string;
  library: ProjectFrameView[] | null;
  onDecided: () => void;
  onOpen: (vm: FrameVM) => void;
}): JSX.Element {
  const [items, setItems] = useState<ModerationFrameView[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [trust, setTrust] = useState(true);
  const [busy, setBusy] = useState(false);
  const [result, setResult] = useState<string | null>(null);
  const [rejecting, setRejecting] = useState<FrameVM[] | null>(null);
  const mounted = useRef(true);

  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
    };
  }, []);

  const load = useCallback(async () => {
    try {
      const next = await api.invoke<ModerationFrameView[]>('list_collab_moderation', { projectId });
      if (mounted.current) setItems(next);
    } catch (err) {
      // S6 — surface a failed load rather than showing a stale/empty queue silently.
      console.error('[moderation] list_collab_moderation failed:', err);
      if (mounted.current) setError(err instanceof Error ? err.message : String(err));
    }
  }, [projectId]);

  useEffect(() => {
    void load();
  }, [load]);

  const mirror = byUuid(library);
  const rows: FrameVM[] = (items ?? []).map((m) => fromModeration(m, mirror));

  const approveAll = useCallback(
    async (targets: FrameVM[]): Promise<void> => {
      setBusy(true);
      setResult(null);
      let done = 0;
      for (const t of targets) {
        try {
          await api.invoke('approve_collab_frame', { projectId, frameUuid: t.frameUuid, trust });
          done += 1;
        } catch (err) {
          const msg = err instanceof Error ? err.message : String(err);
          if (isAlreadyDecided(msg)) {
            // Benign — already decided (another moderator, or this batch's
            // own trust cascade). Not an error: count it and keep going.
            console.info('[moderation] approve_collab_frame: already decided, continuing:', t.frameUuid);
            done += 1;
            continue;
          }
          // Never swallow: log first, then stop and report exactly how far the batch got.
          console.error('[moderation] approve_collab_frame failed:', err);
          if (mounted.current) setResult(`Approved ${done} of ${targets.length} — ${msg}`);
          setBusy(false);
          await load();
          onDecided();
          return;
        }
      }
      setBusy(false);
      await load();
      onDecided();
    },
    [projectId, trust, load, onDecided],
  );

  const rejectAll = useCallback(
    async (targets: FrameVM[], reason: string): Promise<void> => {
      setBusy(true);
      setResult(null);
      let done = 0;
      for (const t of targets) {
        try {
          await api.invoke('reject_collab_frame', { projectId, frameUuid: t.frameUuid, reason });
          done += 1;
        } catch (err) {
          const msg = err instanceof Error ? err.message : String(err);
          if (isAlreadyDecided(msg)) {
            // Benign — see approveAll's matching branch.
            console.info('[moderation] reject_collab_frame: already decided, continuing:', t.frameUuid);
            done += 1;
            continue;
          }
          console.error('[moderation] reject_collab_frame failed:', err);
          if (mounted.current) setResult(`Rejected ${done} of ${targets.length} — ${msg}`);
          setBusy(false);
          setRejecting(null);
          await load();
          onDecided();
          return;
        }
      }
      setBusy(false);
      setRejecting(null);
      await load();
      onDecided();
    },
    [projectId, load, onDecided],
  );

  const actions: TableAction[] = [
    {
      id: 'approve',
      verb: 'Approve',
      eligible: () => true,
      primary: true,
      busy,
      run: (targets) => void approveAll(targets),
    },
    {
      id: 'reject',
      verb: 'Reject',
      eligible: () => true,
      busy,
      run: (targets) => setRejecting(targets),
    },
  ];

  return (
    <div className="space-y-3">
      {error && <p className="text-sm text-error">{error}</p>}
      {result && <p className="text-sm text-error">{result}</p>}

      {items === null ? (
        <p className="text-sm text-content-muted">Loading…</p>
      ) : (
        <ProjectFrameTable
          key={`${projectId}.moderation`}
          tableId="moderation"
          scope={projectId}
          rows={rows}
          actions={actions}
          onOpen={onOpen}
          emptyText="Nothing waiting for review."
          toolbarExtra={
            <Checkbox checked={trust} onChange={setTrust} label="Trust these publishers" size="sm" />
          }
        />
      )}

      {rejecting && (
        <RejectDialog
          frames={rejecting}
          busy={busy}
          onCancel={() => setRejecting(null)}
          onReject={(reason) => void rejectAll(rejecting, reason)}
        />
      )}
    </div>
  );
}

/** Required-reason reject dialog (≤500 chars), moved from `ModerationQueue.tsx`
 *  and generalized to a batch of frames — same rule and copy, one reason
 *  applied to every target. */
function RejectDialog({
  frames,
  busy,
  onCancel,
  onReject,
}: {
  frames: FrameVM[];
  busy: boolean;
  onCancel: () => void;
  onReject: (reason: string) => void;
}) {
  const [reason, setReason] = useState('');
  const trimmed = reason.trim();
  const tooLong = reason.length > REASON_MAX;
  const valid = trimmed.length > 0 && !tooLong;

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape' && !busy) onCancel();
    };
    document.addEventListener('keydown', onKey);
    return () => document.removeEventListener('keydown', onKey);
  }, [onCancel, busy]);

  return (
    <div
      className="fixed inset-0 z-50 flex items-center justify-center bg-black/40"
      onClick={() => !busy && onCancel()}
    >
      <div
        className="w-[30rem] max-w-[90vw] rounded-lg border border-border bg-surface p-4"
        onClick={(e) => e.stopPropagation()}
      >
        <div className="mb-2 flex items-center gap-2">
          <X size={16} className="text-error" />
          <h2 className="font-medium text-content">
            Reject {frames.length} frame{frames.length === 1 ? '' : 's'}
          </h2>
          <button
            onClick={onCancel}
            disabled={busy}
            className="ml-auto text-content-muted transition-colors hover:text-content disabled:opacity-50"
            aria-label="Close"
          >
            <X size={16} />
          </button>
        </div>
        <p className="mb-2 text-xs text-content-muted">The reason is sent to the publisher.</p>
        <textarea
          value={reason}
          onChange={(e) => setReason(e.target.value)}
          rows={4}
          autoFocus
          placeholder="Why is this frame rejected?"
          className="w-full resize-none rounded border border-border bg-surface-elevated p-2 text-sm text-content placeholder:text-content-muted focus:border-accent focus:outline-none"
        />
        <div className="mt-1 flex items-center justify-between text-xs">
          <span className={tooLong ? 'text-error' : 'text-content-muted'}>
            {reason.length}/{REASON_MAX}
          </span>
        </div>
        <div className="mt-3 flex justify-end gap-2">
          <button
            type="button"
            onClick={onCancel}
            disabled={busy}
            className="rounded border border-border px-3 py-1.5 text-sm text-content-secondary transition-colors hover:bg-surface-hover disabled:opacity-50"
          >
            Cancel
          </button>
          <button
            type="button"
            onClick={() => onReject(trimmed)}
            disabled={!valid || busy}
            className="inline-flex items-center gap-1 rounded bg-error px-3 py-1.5 text-sm text-surface transition-colors hover:opacity-90 disabled:cursor-not-allowed disabled:opacity-50"
          >
            {busy && <Loader2 size={12} className="animate-spin" />} Reject
          </button>
        </div>
      </div>
    </div>
  );
}
