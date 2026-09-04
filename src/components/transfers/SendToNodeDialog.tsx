// SendToNodeDialog — the reusable "send these to other nodes" modal (Phase 3,
// explicit app→app send). Entry points render it with a `target`: a frame
// selection (frame-selection toolbars, the analysis view) or a whole frame set
// resolved by an export mode (the Export tab, spec 2026-08-28). It resolves the
// account's other Athenaeum peers, lets the user pick one or more, fans the
// enqueue out via `useSyncSend` (`sendSelection` / `sendFrameSet`), and raises a
// single aggregated outcome notification.
//
// A2 NOTE — destinations are read directly through `api.invoke('list_account_devices')`
// + `api.invoke('account_status')`, NOT the account-state hook or the account settings
// section. That keeps the account-isolation guard grep clean while still surfacing the
// device list (the same offline-resolvable precedent `SyncSection` uses).

import { useEffect, useMemo, useState } from 'react';
import { Send, Check, X, Loader2 } from 'lucide-react';
import { api } from '../../api';
import { useNotifications } from '../../contexts/NotificationContext';
import { useSyncSend, summarizeIneligible, errMsg } from '../../hooks/useSyncSend';
import { formatTimestamp } from '../../utils/dateFormatting';
import type { ExportMode } from '../../types/export';
import type {
  AccountDevice,
  AccountStatus,
  FlatNormMode,
  IneligibleFrame,
  LightCalParams,
} from '../../types/models';

/**
 * The five light-calibration options a `frameSet` send generates its files
 * with (D-1, review fix). Previously this dialog re-read them from
 * `lightCalPrefs.ts` at send time, which could disagree with the Export tab's
 * live (unsaved / not-yet-persisted) state — now every opener supplies the
 * exact values it wants used, from its own live state or, if it has none, the
 * persisted preferences read explicitly at the call site.
 */
export interface LightCalOptions {
  flatNorm: boolean;
  flatNormMode: FlatNormMode;
  params: LightCalParams;
  /** Cosmetic hot-pixel correction from the master dark (default ON). */
  hotPixel: boolean;
  /** Debayer a CFA light to planar RGB (default ON; inert for mono). */
  debayer: boolean;
}

/** Compact display for a hub-assigned device id (opaque, can be long). Mirrors the
 *  helper in the account settings UI — kept local so this file never imports that
 *  module (account-isolation guard). */
function shortId(id: string): string {
  return id.length > 12 ? `${id.slice(0, 8)}…` : id;
}

/**
 * What this send is about. `frames` is the original explicit selection send —
 * the caller already knows the frame ids, and passes `frameSetId` when the
 * selection came from inside an object so the backend uses the WBPP rel_path
 * layout (`null`/absent for a browser selection → source-relative layout).
 * `frameSet` is the Export-tab send (spec 2026-08-28): the backend resolves the
 * set's files itself from `mode`, so the dialog only carries what it must show —
 * the mode's label and the file count the tab displayed.
 */
export type SendToNodeTarget =
  | { kind: 'frames'; frameIds: number[]; frameSetId?: number | null }
  | { kind: 'frameSet'; frameSetId: number; mode: ExportMode; modeLabel: string; fileCount: number };

interface SendToNodeDialogProps {
  /** What to send. The Send button stays disabled while this resolves to nothing. */
  target: SendToNodeTarget;
  /** Whether the modal is mounted/visible. */
  open: boolean;
  /** Close the modal (also called after a successful send). */
  onClose: () => void;
  /**
   * Pre-fill for the editable "Transfer name" field (§D1). Object sends pass the
   * frame-set name; the browser passes the selection's common folder name; blank
   * → the field starts empty and the backend auto-names.
   */
  defaultBatchName?: string;
  /**
   * The light-calibration options a `frameSet` send generates its files with
   * (D-1, review fix). Always required, even for a `frames`-kind send that
   * never reaches `sendFrameSet` — every opener names its source (live state
   * or explicit persisted-prefs read) rather than the dialog silently
   * re-reading localStorage behind the caller's back.
   */
  lightCalOptions: LightCalOptions;
}

/** Explanatory empty state — signed out, or no eligible peers on the account. */
function EmptyState({ message }: { message: string }) {
  return (
    <div className="text-xs text-content-muted py-6 px-1 text-center leading-relaxed">
      {message}
    </div>
  );
}

export function SendToNodeDialog({
  target,
  open,
  onClose,
  defaultBatchName,
  lightCalOptions,
}: SendToNodeDialogProps) {
  const { notify } = useNotifications();
  const { sending, sendSelection, sendFrameSet } = useSyncSend();

  // A selection send counts frames; a frame-set send counts the files the Export
  // tab resolved for the chosen mode (the same number the backend reports back).
  const itemCount = target.kind === 'frames' ? target.frameIds.length : target.fileCount;
  const itemNoun = target.kind === 'frames' ? 'frame' : 'file';
  const subtitle = target.kind === 'frameSet' ? ` — ${target.modeLabel}` : '';

  const [loading, setLoading] = useState(true);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [signedIn, setSignedIn] = useState(false);
  const [candidates, setCandidates] = useState<AccountDevice[]>([]);
  const [checked, setChecked] = useState<Set<string>>(new Set());
  const [batchName, setBatchName] = useState('');

  // Reset the editable name to the caller's suggestion each open (§D1). Blank
  // when no suggestion → the backend auto-names.
  useEffect(() => {
    if (open) setBatchName(defaultBatchName ?? '');
  }, [open, defaultBatchName]);

  // Resolve destinations each time the dialog opens. StrictMode double-mounts in
  // dev, so guard the async resolve with a cancelled flag.
  useEffect(() => {
    if (!open) return;
    let cancelled = false;
    setLoading(true);
    setLoadError(null);
    setChecked(new Set());
    Promise.all([
      api.invoke<AccountDevice[]>('list_account_devices'),
      api.invoke<AccountStatus>('account_status'),
    ])
      .then(([devices, status]) => {
        if (cancelled) return;
        setSignedIn(status.signedIn);
        // Candidates = other full-peer Athenaeum nodes on this account (never
        // Perseus send-only agents, never self). The backend re-validates +
        // rejects Perseus and unknown devices too; self is excluded only by
        // this UI filter, so this is a UX filter, not the security boundary.
        const cands = status.signedIn
          ? devices.filter((d) => d.capability === 'athenaeum' && d.id !== status.deviceId)
          : [];
        setCandidates(cands);
        setLoading(false);
      })
      .catch((err) => {
        if (cancelled) return;
        console.error('[sync] failed to load send destinations:', err);
        setLoadError(errMsg(err));
        setLoading(false);
      });
    return () => {
      cancelled = true;
    };
  }, [open]);

  const nameFor = useMemo(() => {
    const byId = new Map(candidates.map((d) => [d.id, d.name] as const));
    return (id: string) => byId.get(id) ?? shortId(id);
  }, [candidates]);

  const checkedCount = checked.size;
  const hasList = !loading && !loadError && signedIn && candidates.length > 0;

  const toggle = (id: string) => {
    setChecked((prev) => {
      const next = new Set(prev);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });
  };

  const handleSend = async () => {
    const checkedIds = [...checked];
    if (checkedIds.length === 0 || itemCount === 0 || sending) return;

    // A frame-set send passes ALL FIVE light-calibration options the opener
    // supplied via `lightCalOptions` (D-1) — the options the calibrated-lights
    // mode generates its files with (the readiness gate itself no longer takes
    // any). The transfer generates the same bytes an export of this set would.
    const results =
      target.kind === 'frames'
        ? await sendSelection(target.frameIds, checkedIds, {
            batchName,
            frameSetId: target.frameSetId ?? null,
          })
        : await sendFrameSet(target.frameSetId, target.mode, checkedIds, {
            batchName,
            ...lightCalOptions,
          });

    // --- Aggregate the per-destination outcomes into one honest notification. ---
    const total = target.kind === 'frames' ? target.frameIds.length : target.fileCount;
    const nodeCount = checkedIds.length;
    const queued = results.reduce((sum, r) => sum + (r.result?.enqueuedCount ?? 0), 0);
    const failedNodes = results.filter((r) => r.error);

    // Ineligibility is a per-frame property (present on disk + resolvable in the
    // catalog), re-validated identically for every destination — so dedupe by
    // frameId to get the TRUE count rather than count × nodes.
    const ineligByFrame = new Map<number, string>();
    for (const r of results) {
      for (const f of r.result?.ineligible ?? []) ineligByFrame.set(f.frameId, f.reason);
    }
    const ineligible: IneligibleFrame[] = [...ineligByFrame.entries()].map(([frameId, reason]) => ({
      frameId,
      reason,
    }));
    const eligible = total - ineligible.length;

    // Full success = every checked node accepted every frame (no transport failure,
    // nothing ineligible). Equivalent to `queued === total * nodeCount`.
    const allOk = failedNodes.length === 0 && queued === total * nodeCount;

    const detailParts: string[] = [];
    if (ineligible.length > 0) detailParts.push(summarizeIneligible(ineligible));
    if (failedNodes.length > 0) {
      const names = failedNodes.map((r) => nameFor(r.deviceId));
      const shown = names.slice(0, 3).join(', ');
      const extra = names.length > 3 ? ` +${names.length - 3} more` : '';
      detailParts.push(`${shown}${extra} failed`);
    }

    notify({
      kind: 'sync',
      tone: allOk ? 'success' : 'warning',
      hasErrors: failedNodes.length > 0 || queued === 0,
      title: allOk
        ? `Queued ${total} ${itemNoun}${total === 1 ? '' : 's'} to ${nodeCount} node${nodeCount === 1 ? '' : 's'}`
        : `Queued ${eligible} of ${total} ${itemNoun}${total === 1 ? '' : 's'} to ${nodeCount} node${nodeCount === 1 ? '' : 's'}`,
      detail:
        detailParts.length > 0
          ? detailParts.join(' · ')
          : `Sending to ${nodeCount} node${nodeCount === 1 ? '' : 's'}.`,
    });

    onClose();
  };

  if (!open) return null;

  return (
    <div
      className="fixed inset-0 bg-black/50 flex items-center justify-center z-50"
      onClick={onClose}
    >
      <div
        className="bg-surface-elevated rounded-lg border border-border w-[420px] max-h-[80vh] overflow-y-auto p-4"
        onClick={(e) => e.stopPropagation()}
        role="dialog"
        aria-modal="true"
        aria-label="Send frames to nodes"
      >
        {/* Header */}
        <div className="flex items-center justify-between mb-3">
          <h3 className="text-sm font-medium text-content flex items-center gap-2">
            <Send size={16} className="text-accent" />
            Send {itemCount} {itemNoun}{itemCount === 1 ? '' : 's'}{subtitle}
          </h3>
          <button
            onClick={onClose}
            className="text-content-muted hover:text-content"
            aria-label="Close"
          >
            <X size={16} />
          </button>
        </div>

        {/* Body */}
        {loading ? (
          <div className="flex items-center justify-center gap-2 text-xs text-content-muted py-6">
            <Loader2 size={14} className="animate-spin" /> Loading destinations…
          </div>
        ) : loadError ? (
          <div className="text-xs text-error py-4">{loadError}</div>
        ) : !signedIn ? (
          <EmptyState message="Sign in and add an Athenaeum device to this account to send frames to it." />
        ) : candidates.length === 0 ? (
          <EmptyState message="No other Athenaeum nodes on this account. Add another device to send frames to it." />
        ) : (
          <>
            <p className="text-xs text-content-muted mb-2">
              Choose which node{candidates.length === 1 ? '' : 's'} receive the{' '}
              {target.kind === 'frames' ? 'selected frames' : 'frame set'}.
            </p>
            <div className="space-y-0.5 mb-3">
              {candidates.map((d) => {
                const isChecked = checked.has(d.id);
                return (
                  <label
                    key={d.id}
                    className="flex items-center gap-2.5 px-2 py-1.5 rounded hover:bg-surface-hover cursor-pointer"
                  >
                    <input
                      type="checkbox"
                      checked={isChecked}
                      onChange={() => toggle(d.id)}
                      className="peer sr-only"
                    />
                    <span
                      className={`flex items-center justify-center w-4 h-4 shrink-0 rounded border transition-colors peer-focus-visible:ring-2 peer-focus-visible:ring-accent/50 ${
                        isChecked ? 'bg-accent border-accent' : 'border-border bg-surface'
                      }`}
                    >
                      {isChecked && <Check size={12} className="text-white" />}
                    </span>
                    <div className="min-w-0 flex-1">
                      <div className="text-sm text-content truncate">{d.name}</div>
                      <div className="font-mono text-[11px] text-content-muted truncate">
                        {shortId(d.id)}
                        {d.lastSeenAt ? ` · seen ${formatTimestamp(d.lastSeenAt)}` : ''}
                      </div>
                    </div>
                  </label>
                );
              })}
            </div>

            {/* Transfer name (§D1) — pre-filled, editable; blank sends omit it
                so the backend auto-names the batch. */}
            <label className="mb-3 block">
              <span className="mb-1 block text-xs text-content-muted">Transfer name</span>
              <input
                type="text"
                value={batchName}
                onChange={(e) => setBatchName(e.target.value)}
                placeholder="Auto-named if left blank"
                className="w-full rounded border border-border bg-surface px-2 py-1.5 text-sm text-content placeholder:text-content-muted focus:border-accent focus:outline-none"
              />
            </label>
          </>
        )}

        {/* Footer — only meaningful when there's a selectable list. */}
        {hasList && (
          <div className="flex justify-end gap-2 pt-1">
            <button
              onClick={onClose}
              className="px-3 py-1.5 text-sm text-content-secondary hover:bg-surface-hover rounded"
            >
              Cancel
            </button>
            <button
              onClick={handleSend}
              disabled={sending || checkedCount === 0 || itemCount === 0}
              className="px-3 py-1.5 bg-accent hover:bg-accent-hover text-white text-sm rounded disabled:opacity-50 disabled:cursor-not-allowed inline-flex items-center gap-1.5"
            >
              {sending ? <Loader2 size={14} className="animate-spin" /> : <Send size={14} />}
              {sending
                ? 'Sending…'
                : `Send to ${checkedCount} node${checkedCount === 1 ? '' : 's'}`}
            </button>
          </div>
        )}
      </div>
    </div>
  );
}
