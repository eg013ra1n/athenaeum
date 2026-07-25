// Settings → Sync (task M2b). Renders inner content only — the host card/heading
// are supplied by `Settings.tsx`, matching the `AccountSection` / `LoggingSettings`
// pattern and placed right after Account.
//
// A2 NOTE — like `useSyncSend`, this reads the offline-resolvable `account_status`
// command directly (for signed-in) and does NOT import `useAccount` /
// `AccountSection`, so account state proper stays isolated in those two files. A
// signed-out user sees a quiet "sign in to configure sync" empty state and no sync
// code runs. Every status is guarded against null/undefined — nothing here throws.

import { useCallback, useEffect, useRef, useState } from 'react';
import {
  Loader2,
  Inbox,
  AlertTriangle,
  ChevronRight,
  ChevronDown,
  Copy,
  Check,
  Trash2,
  Save,
} from 'lucide-react';
import { api } from '../../api';
import { formatBytes } from '../transfers/presentation';
import { useNotifications } from '../../contexts/NotificationContext';
import type { AccountStatus, SyncStatus, TransferStorage, TransferCleanup } from '../../types/models';

/** Tauri and Axum both reject with a plain string, not an `Error`. */
function errMsg(err: unknown): string {
  return err instanceof Error ? err.message : String(err);
}

// Upload speed limit (W1). The setting `sync.max_upload_bytes_per_sec` is stored
// as BYTES per second ("0" = unlimited); the field shows DECIMAL megabytes per
// second — 1 MB/s = 1_000_000 bytes/s, the convention ISPs and network gear use,
// NOT 1 MiB/s = 1_048_576. Keep the two directions symmetric.
const BYTES_PER_MB = 1_000_000;

/** Client mirror of the server floor (100000 bytes/s), so the common mistake
 *  gets an inline answer instead of a round-trip error. */
const MIN_LIMIT_MB = 0.1;

/** bytes/s (as stored, a string) → the MB/s text shown in the field. `0` and
 *  anything unparseable render as empty, which the field labels "Unlimited". */
function bytesToMbInput(raw: string): string {
  const bytes = Number(raw);
  if (!Number.isFinite(bytes) || bytes <= 0) return '';
  // Trim float artifacts: 500000 / 1e6 must read "0.5", not "0.5000000000000001".
  return String(Number((bytes / BYTES_PER_MB).toFixed(3)));
}

// Simultaneous incoming transfers (W2 T2.7). `sync.max_concurrent_receives` is
// stored as a plain integer string; the server accepts 1..=8 and the receiver's
// getter clamps into the same window. Mirror both here so a bad value never
// round-trips and the field never shows a cap the receiver isn't using.
const MIN_RECEIVES = 1;
const MAX_RECEIVES = 8;
const DEFAULT_RECEIVES = '2';

/** Stored value → the integer text shown in the field, clamped the same way the
 *  receiver clamps it (`SettingsManager::get_sync_max_concurrent_receives`), so
 *  a hand-edited or migrated row displays the cap actually in force. Anything
 *  unparseable falls back to the default. */
function receivesToInput(raw: string): string {
  const n = Number(raw);
  if (!Number.isFinite(n) || !Number.isInteger(n)) return DEFAULT_RECEIVES;
  return String(Math.min(MAX_RECEIVES, Math.max(MIN_RECEIVES, n)));
}

export default function SyncSection() {
  const mounted = useRef(true);
  const { notify } = useNotifications();

  const [status, setStatus] = useState<AccountStatus | null>(null);
  const [loadingStatus, setLoadingStatus] = useState(true);

  const [syncStatus, setSyncStatus] = useState<SyncStatus | null>(null);
  const [devFlag, setDevFlag] = useState(false);

  // Transfer storage (B7): the on-disk footprint of the transfer temp data +
  // the one-click "clean up finished transfers" reclaim.
  const [storage, setStorage] = useState<TransferStorage | null>(null);
  const [cleaning, setCleaning] = useState(false);

  // Upload speed limit (W1) — text-backed, not number-backed, so the field can
  // legitimately be empty (= unlimited). `savedUploadMb` is the last persisted
  // value and drives the Save button's dirty state.
  const [uploadMb, setUploadMb] = useState('');
  const [savedUploadMb, setSavedUploadMb] = useState('');
  const [uploadError, setUploadError] = useState<string | null>(null);
  const [savingUpload, setSavingUpload] = useState(false);

  // Simultaneous incoming transfers (W2 T2.7) — same text-backed shape as the
  // upload limit: `savedReceives` is the last persisted value and drives the
  // Save button's dirty state.
  const [receives, setReceives] = useState(DEFAULT_RECEIVES);
  const [savedReceives, setSavedReceives] = useState(DEFAULT_RECEIVES);
  const [receivesError, setReceivesError] = useState<string | null>(null);
  const [savingReceives, setSavingReceives] = useState(false);

  // Dev pairing-ticket disclosure — lazily fetched on first expand.
  const [showTicket, setShowTicket] = useState(false);
  const [ticket, setTicket] = useState<string | null>(null);
  const [ticketError, setTicketError] = useState<string | null>(null);
  const [ticketLoading, setTicketLoading] = useState(false);
  const [copied, setCopied] = useState(false);
  const [copyFailed, setCopyFailed] = useState(false);

  const signedIn = status?.signedIn ?? false;

  // Account status — offline-resolvable, so a failure is unexpected: log, degrade.
  useEffect(() => {
    mounted.current = true;
    (async () => {
      try {
        const s = await api.invoke<AccountStatus>('account_status');
        if (mounted.current) setStatus(s);
      } catch (err) {
        console.error('[sync] account status poll failed:', err);
      } finally {
        if (mounted.current) setLoadingStatus(false);
      }
    })();
    return () => {
      mounted.current = false;
    };
  }, []);

  // Transfer-storage footprint — best-effort, degrades to null on failure.
  const refreshStorage = useCallback(async () => {
    try {
      const s = await api.invoke<TransferStorage>('get_transfer_storage');
      if (mounted.current) setStorage(s ?? null);
    } catch (err) {
      console.error('[sync] transfer storage poll failed:', err);
    }
  }, []);

  // Sync-side state loads only once signed in.
  useEffect(() => {
    if (!signedIn) {
      setSyncStatus(null);
      setStorage(null);
      setUploadMb('');
      setSavedUploadMb('');
      setUploadError(null);
      setReceives(DEFAULT_RECEIVES);
      setSavedReceives(DEFAULT_RECEIVES);
      setReceivesError(null);
      return;
    }
    (async () => {
      try {
        const [ss, devVal, uploadVal, receivesVal] = await Promise.all([
          api.invoke<SyncStatus>('get_sync_status'),
          api.invoke<string>('get_setting', {
            key: 'sync.dev_ticket_pairing',
            defaultValue: 'false',
          }),
          // Settings cross the boundary as STRINGS; "0" is the unlimited sentinel.
          api.invoke<string>('get_setting', {
            key: 'sync.max_upload_bytes_per_sec',
            defaultValue: '0',
          }),
          // No dedicated getter command — the cap is read through generic
          // `get_setting`, same default ("2") the backend applies.
          api.invoke<string>('get_setting', {
            key: 'sync.max_concurrent_receives',
            defaultValue: DEFAULT_RECEIVES,
          }),
        ]);
        if (!mounted.current) return;
        setSyncStatus(ss ?? null);
        setDevFlag(devVal.toLowerCase() === 'true');
        const mb = bytesToMbInput(uploadVal ?? '0');
        setUploadMb(mb);
        setSavedUploadMb(mb);
        const lanes = receivesToInput(receivesVal ?? DEFAULT_RECEIVES);
        setReceives(lanes);
        setSavedReceives(lanes);
      } catch (err) {
        console.error('[sync] load sync settings failed:', err);
      }
    })();
    refreshStorage();
  }, [signedIn, refreshStorage]);

  const handleCleanup = async () => {
    setCleaning(true);
    try {
      const result = await api.invoke<TransferCleanup>('cleanup_finished_transfers');
      // D2: payload dirs are the SEND side, staging trees the RECEIVE side — a
      // receive-only device only ever has the latter, so both are reported. Freed
      // bytes are the two together; released downloads are the delayed half.
      const freedBytes = result.payloadBytes + result.stagingBytes;
      const parts: string[] = [];
      if (result.payloadDirs > 0) {
        parts.push(`${result.payloadDirs} package${result.payloadDirs === 1 ? '' : 's'}`);
      }
      if (result.stagingDirs > 0) {
        parts.push(`${result.stagingDirs} received batch${result.stagingDirs === 1 ? '' : 'es'}`);
      }
      const what = parts.length > 0 ? ` (${parts.join(', ')})` : '';
      const tags =
        result.tagsReleased > 0
          ? `, released ${result.tagsReleased} partial download${result.tagsReleased === 1 ? '' : 's'} — those bytes return within about 15 minutes`
          : '';
      notify({
        title: 'Finished transfers cleaned up',
        detail: `Freed ${formatBytes(freedBytes)}${what}${tags}`,
        kind: 'sync',
        tone: 'success',
      });
      await refreshStorage();
    } catch (err) {
      console.error('[sync] cleanup finished transfers failed:', err);
      notify({
        title: 'Cleanup failed',
        detail: errMsg(err),
        kind: 'sync',
        tone: 'warning',
      });
    } finally {
      if (mounted.current) setCleaning(false);
    }
  };

  const handleSaveUploadLimit = async () => {
    const raw = uploadMb.trim();
    // Empty or 0 → unlimited. Anything else must parse and clear the same floor
    // the server enforces, checked here so the common case never round-trips.
    let bytesPerSec = 0;
    if (raw !== '') {
      const mbps = Number(raw);
      if (!Number.isFinite(mbps) || mbps < 0) {
        setUploadError('Enter a number in MB/s, or leave the field empty for unlimited.');
        return;
      }
      if (mbps > 0 && mbps < MIN_LIMIT_MB) {
        setUploadError(
          `Minimum limit is ${MIN_LIMIT_MB} MB/s. Use 0 (or leave empty) for unlimited.`,
        );
        return;
      }
      bytesPerSec = Math.round(mbps * BYTES_PER_MB);
      if (!Number.isSafeInteger(bytesPerSec)) {
        setUploadError('That limit is too large — enter a realistic MB/s value.');
        return;
      }
    }
    setUploadError(null);
    setSavingUpload(true);
    try {
      await api.invoke('set_sync_upload_limit', { bytesPerSec });
      // Canonicalise the field to what was actually stored (0 → empty = Unlimited).
      const shown = bytesToMbInput(String(bytesPerSec));
      if (mounted.current) {
        setUploadMb(shown);
        setSavedUploadMb(shown);
      }
      notify({
        title: 'Upload speed limit saved',
        detail:
          bytesPerSec === 0
            ? 'Sync uploads from this device are unlimited.'
            : `Sync uploads from this device are capped at ${shown} MB/s.`,
        kind: 'sync',
        tone: 'success',
      });
    } catch (err) {
      console.error('[sync] set upload limit failed:', err);
      const msg = errMsg(err);
      if (mounted.current) setUploadError(msg);
      notify({
        title: 'Could not save upload speed limit',
        detail: msg,
        kind: 'sync',
        tone: 'warning',
      });
    } finally {
      if (mounted.current) setSavingUpload(false);
    }
  };

  const handleSaveConcurrentReceives = async () => {
    const raw = receives.trim();
    const n = Number(raw);
    // Client mirror of the server's 1..=8 guard, so a typo answers inline
    // instead of round-tripping. Whole numbers only — there is no half a lane.
    if (raw === '' || !Number.isFinite(n) || !Number.isInteger(n)) {
      setReceivesError(`Enter a whole number between ${MIN_RECEIVES} and ${MAX_RECEIVES}.`);
      return;
    }
    if (n < MIN_RECEIVES || n > MAX_RECEIVES) {
      setReceivesError(`Must be between ${MIN_RECEIVES} and ${MAX_RECEIVES}.`);
      return;
    }
    setReceivesError(null);
    setSavingReceives(true);
    try {
      await api.invoke('set_sync_max_concurrent_receives', { maxConcurrentReceives: n });
      const shown = String(n);
      if (mounted.current) {
        setReceives(shown);
        setSavedReceives(shown);
      }
      notify({
        title: 'Simultaneous incoming transfers saved',
        detail:
          n === 1
            ? 'Incoming transfers download one at a time.'
            : `Up to ${n} incoming transfers download at once.`,
        kind: 'sync',
        tone: 'success',
      });
    } catch (err) {
      console.error('[sync] set max concurrent receives failed:', err);
      const msg = errMsg(err);
      if (mounted.current) setReceivesError(msg);
      notify({
        title: 'Could not save simultaneous incoming transfers',
        detail: msg,
        kind: 'sync',
        tone: 'warning',
      });
    } finally {
      if (mounted.current) setSavingReceives(false);
    }
  };

  const handleToggleTicket = async () => {
    const next = !showTicket;
    setShowTicket(next);
    if (next && ticket === null && !ticketLoading) {
      setTicketLoading(true);
      setTicketError(null);
      try {
        const t = await api.invoke<string>('get_sync_pairing_ticket');
        if (mounted.current) setTicket(t);
      } catch (err) {
        // Dev-gated: a disabled flag rejects here. Show quietly, never throw.
        console.error('[sync] get pairing ticket failed:', err);
        if (mounted.current) setTicketError(errMsg(err));
      } finally {
        if (mounted.current) setTicketLoading(false);
      }
    }
  };

  const handleCopyTicket = async () => {
    if (!ticket) return;
    // `navigator.clipboard?.writeText` short-circuits to `undefined` when the
    // API is absent (insecure context / older webview) — awaiting it resolves
    // successfully and would falsely report "Copied". Require the real method,
    // fall back to the legacy execCommand path, and surface an honest failure
    // when neither works. Never claim success without actually copying.
    setCopyFailed(false);
    let ok = false;
    try {
      if (navigator.clipboard?.writeText) {
        await navigator.clipboard.writeText(ticket);
        ok = true;
      } else if (typeof document !== 'undefined' && document.execCommand) {
        const ta = document.createElement('textarea');
        ta.value = ticket;
        ta.style.position = 'fixed';
        ta.style.opacity = '0';
        document.body.appendChild(ta);
        ta.select();
        ok = document.execCommand('copy');
        document.body.removeChild(ta);
      }
    } catch (err) {
      console.error('[sync] copy ticket failed:', err);
      ok = false;
    }
    if (!mounted.current) return;
    if (ok) {
      setCopied(true);
      setTimeout(() => {
        if (mounted.current) setCopied(false);
      }, 2000);
    } else {
      console.error('[sync] copy ticket failed: clipboard unavailable');
      setCopyFailed(true);
      setTimeout(() => {
        if (mounted.current) setCopyFailed(false);
      }, 2000);
    }
  };

  // ── render ───────────────────────────────────────────────────────────────────

  if (loadingStatus) {
    return (
      <div className="flex items-center gap-2 text-sm text-content-muted">
        <Loader2 size={16} className="animate-spin" />
        Loading sync…
      </div>
    );
  }

  // SIGNED OUT — quiet empty state (A2 additive rule: the app works signed-out).
  if (!signedIn) {
    return (
      <p className="text-sm text-content-muted">
        Sign in to configure sync. Use the <span className="text-content-secondary">Account</span>{' '}
        section above to link this machine to your account.
      </p>
    );
  }

  return (
    <div className="space-y-6">
      {/* Receiver status */}
      <div>
        <h4 className="text-sm font-medium text-content-secondary mb-2">Status</h4>
        <div className="space-y-1.5 text-sm text-content-muted">
          <div className="flex items-center gap-2">
            <Inbox size={14} className="flex-shrink-0" />
            Receiver{' '}
            <span className={syncStatus?.transportStarted ? 'text-success' : 'text-content-secondary'}>
              {syncStatus?.transportStarted ? 'active' : 'idle'}
            </span>
            <span className="text-content-muted">·</span>
            <span className="text-content-secondary">{syncStatus?.receivedTotal ?? 0}</span> frames
            received
          </div>
          <p className="text-xs text-content-muted">
            This machine is a full peer: it always receives, and sends are explicit and per-device.
          </p>
        </div>
      </div>

      {/* Transfer storage (B7): footprint + one-click reclaim of finished-transfer temp data. */}
      <div>
        <h4 className="text-sm font-medium text-content-secondary mb-2">Transfer storage</h4>
        <div className="flex items-center justify-between gap-3">
          <p className="text-sm text-content-muted">
            {storage ? (
              <>
                <span className="text-content-secondary">{storage.packagesCount}</span> package
                {storage.packagesCount === 1 ? '' : 's'} ·{' '}
                <span className="text-content-secondary">{formatBytes(storage.packagesBytes)}</span> on
                disk · received{' '}
                <span className="text-content-secondary">{formatBytes(storage.stagingBytes)}</span> ·
                blobs <span className="text-content-secondary">{formatBytes(storage.blobsBytes)}</span>
              </>
            ) : (
              'Calculating…'
            )}
          </p>
          <button
            type="button"
            onClick={handleCleanup}
            disabled={cleaning}
            className="flex-shrink-0 inline-flex items-center gap-1.5 rounded-md border border-border px-2.5 py-1.5 text-xs text-content-secondary hover:bg-surface-hover disabled:opacity-50 transition-colors"
          >
            {cleaning ? (
              <Loader2 size={13} className="animate-spin" />
            ) : (
              <Trash2 size={13} />
            )}
            Clean up finished transfers
          </button>
        </div>
        <p className="mt-1.5 text-xs text-content-muted">
          Removes finished transfers' temporary payloads and releases orphaned download data.
          Received files and transfer history are untouched.
        </p>
      </div>

      {/* Upload speed limit (W1): one device-wide cap on sync UPLOAD bandwidth.
          Shown in decimal MB/s, stored as bytes/s; empty or 0 = unlimited. */}
      <div>
        <h4 className="text-sm font-medium text-content-secondary mb-2">Upload speed limit</h4>
        <div className="flex items-center gap-3">
          <div className="flex items-center gap-2">
            <input
              type="number"
              inputMode="decimal"
              value={uploadMb}
              onChange={(e) => {
                setUploadMb(e.target.value);
                setUploadError(null);
              }}
              onKeyDown={(e) => {
                if (e.key === 'Enter') handleSaveUploadLimit();
              }}
              step="0.1"
              min="0"
              placeholder="Unlimited"
              aria-label="Upload speed limit in megabytes per second"
              className={`w-32 rounded-md border bg-surface-hover px-2.5 py-1.5 text-sm text-content focus:outline-none focus:border-accent transition-colors ${
                uploadError ? 'border-error' : 'border-border'
              }`}
            />
            <span className="text-sm text-content-muted">MB/s</span>
          </div>
          <button
            type="button"
            onClick={handleSaveUploadLimit}
            disabled={savingUpload || uploadMb.trim() === savedUploadMb}
            className="flex-shrink-0 inline-flex items-center gap-1.5 rounded-md border border-border px-2.5 py-1.5 text-xs text-content-secondary hover:bg-surface-hover disabled:opacity-50 transition-colors"
          >
            {savingUpload ? (
              <Loader2 size={13} className="animate-spin" />
            ) : (
              <Save size={13} />
            )}
            Save
          </button>
        </div>
        {uploadError && <p className="mt-1.5 text-xs text-error">{uploadError}</p>}
        <p className="mt-1.5 text-xs text-content-muted">
          Caps this device's total sync upload bandwidth. Uploads only — downloads are capped by
          the sending device's limit. Empty or <span className="text-content-secondary">0</span>{' '}
          means unlimited.
        </p>
      </div>

      {/* Simultaneous incoming transfers (W2 T2.7): how many inbound transfers
          download at once. Integer 1..=8, live-applied by the receive gate. */}
      <div>
        <h4 className="text-sm font-medium text-content-secondary mb-2">
          Simultaneous incoming transfers
        </h4>
        <div className="flex items-center gap-3">
          <input
            type="number"
            inputMode="numeric"
            value={receives}
            onChange={(e) => {
              setReceives(e.target.value);
              setReceivesError(null);
            }}
            onKeyDown={(e) => {
              if (e.key === 'Enter') handleSaveConcurrentReceives();
            }}
            step="1"
            min={MIN_RECEIVES}
            max={MAX_RECEIVES}
            aria-label="Number of simultaneous incoming transfers"
            className={`w-20 rounded-md border bg-surface-hover px-2.5 py-1.5 text-sm text-content focus:outline-none focus:border-accent transition-colors ${
              receivesError ? 'border-error' : 'border-border'
            }`}
          />
          <button
            type="button"
            onClick={handleSaveConcurrentReceives}
            disabled={savingReceives || receives.trim() === savedReceives}
            className="flex-shrink-0 inline-flex items-center gap-1.5 rounded-md border border-border px-2.5 py-1.5 text-xs text-content-secondary hover:bg-surface-hover disabled:opacity-50 transition-colors"
          >
            {savingReceives ? (
              <Loader2 size={13} className="animate-spin" />
            ) : (
              <Save size={13} />
            )}
            Save
          </button>
        </div>
        {receivesError && <p className="mt-1.5 text-xs text-error">{receivesError}</p>}
        <p className="mt-1.5 text-xs text-content-muted">
          How many incoming transfers download at once. Others wait their turn — transfers from the
          same device always arrive in order. Default{' '}
          <span className="text-content-secondary">2</span>; raise it only if this machine's disk
          keeps up.
        </p>
      </div>

      {/* Dev pairing ticket — only when the dev flag is on (sync.dev_ticket_pairing). */}
      {devFlag && (
        <div className="rounded-lg border border-border bg-surface p-3">
          <button
            type="button"
            onClick={handleToggleTicket}
            className="flex items-center gap-1.5 text-sm text-content-secondary hover:text-content transition-colors"
          >
            {showTicket ? <ChevronDown size={14} /> : <ChevronRight size={14} />}
            Show pairing ticket (dev)
          </button>
          {showTicket && (
            <div className="mt-3">
              {ticketLoading ? (
                <div className="flex items-center gap-2 text-sm text-content-muted">
                  <Loader2 size={14} className="animate-spin" />
                  Starting receiver…
                </div>
              ) : ticketError ? (
                <div className="flex items-start gap-2 rounded-lg border border-warning/50 bg-warning-muted p-2.5">
                  <AlertTriangle size={16} className="text-warning flex-shrink-0 mt-0.5" />
                  <p className="text-sm text-warning/90">{ticketError}</p>
                </div>
              ) : ticket ? (
                <div className="space-y-2">
                  <p className="text-xs text-content-muted">
                    Share this ticket with a device (e.g. a Perseus agent) so it can dial this
                    machine. Dev-only pairing path.
                  </p>
                  <div className="flex items-start gap-2">
                    <code className="flex-1 min-w-0 break-all rounded bg-surface-hover border border-border px-2 py-1.5 text-xs font-mono text-content-secondary">
                      {ticket}
                    </code>
                    <button
                      type="button"
                      onClick={handleCopyTicket}
                      title={copyFailed ? 'Copy failed — select the ticket and copy manually' : 'Copy ticket'}
                      className={`flex-shrink-0 inline-flex items-center gap-1 rounded-md border px-2 py-1.5 text-xs transition-colors ${
                        copyFailed
                          ? 'border-warning/50 text-warning hover:bg-warning-muted'
                          : 'border-border text-content-secondary hover:bg-surface-hover'
                      }`}
                    >
                      {copyFailed ? (
                        <AlertTriangle size={13} className="text-warning" />
                      ) : copied ? (
                        <Check size={13} className="text-success" />
                      ) : (
                        <Copy size={13} />
                      )}
                      {copyFailed ? 'Copy failed' : copied ? 'Copied' : 'Copy'}
                    </button>
                  </div>
                </div>
              ) : (
                <p className="text-sm text-content-muted">No ticket available.</p>
              )}
            </div>
          )}
        </div>
      )}
    </div>
  );
}
