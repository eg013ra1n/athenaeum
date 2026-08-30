// Settings → Sync (task M2b). Renders inner content only — the host card/heading
// are supplied by `Settings.tsx`, matching the `AccountSection` / `LoggingSettings`
// pattern and placed right after Account.
//
// A2 NOTE — like `useSyncSend`, this reads the offline-resolvable `account_status`
// command directly (for signed-in) and does NOT import `useAccount` /
// `AccountSection`, so account state proper stays isolated in those two files. A
// signed-out user sees a quiet "sign in to configure sync" empty state and no sync
// code runs. Every status is guarded against null/undefined — nothing here throws.
//
// The transfer folders, upload limit, receive concurrency and transfer-storage
// cards live in Settings → Transfers (`TransfersSection`); this section is
// account status + pairing.

import { useEffect, useRef, useState } from 'react';
import {
  Loader2,
  Inbox,
  AlertTriangle,
  ChevronRight,
  ChevronDown,
  Copy,
  Check,
} from 'lucide-react';
import { api } from '../../api';
import type { AccountStatus, SyncStatus } from '../../types/models';

/** Tauri and Axum both reject with a plain string, not an `Error`. */
function errMsg(err: unknown): string {
  return err instanceof Error ? err.message : String(err);
}

export default function SyncSection() {
  const mounted = useRef(true);

  const [status, setStatus] = useState<AccountStatus | null>(null);
  const [loadingStatus, setLoadingStatus] = useState(true);

  const [syncStatus, setSyncStatus] = useState<SyncStatus | null>(null);
  const [devFlag, setDevFlag] = useState(false);

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

  // Sync-side state loads only once signed in.
  useEffect(() => {
    if (!signedIn) {
      setSyncStatus(null);
      return;
    }
    (async () => {
      try {
        const [ss, devVal] = await Promise.all([
          api.invoke<SyncStatus>('get_sync_status'),
          api.invoke<string>('get_setting', {
            key: 'sync.dev_ticket_pairing',
            defaultValue: 'false',
          }),
        ]);
        if (!mounted.current) return;
        setSyncStatus(ss ?? null);
        setDevFlag(devVal.toLowerCase() === 'true');
      } catch (err) {
        console.error('[sync] load sync settings failed:', err);
      }
    })();
  }, [signedIn]);

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
