// Settings → Sync (task M2b). Renders inner content only — the host card/heading
// are supplied by `Settings.tsx`, matching the `AccountSection` / `LoggingSettings`
// pattern and placed right after Account.
//
// A2 NOTE — like `useSyncSend`, this reads the offline-resolvable `account_status`
// command directly (for signed-in + role) and does NOT import `useAccount` /
// `AccountSection`, so account state proper stays isolated in those two files. A
// signed-out user sees a quiet "sign in to configure sync" empty state and no sync
// code runs. Every status is guarded against null/undefined — nothing here throws.

import { useEffect, useRef, useState } from 'react';
import {
  Loader2,
  Monitor,
  Inbox,
  AlertTriangle,
  ChevronRight,
  ChevronDown,
  Copy,
  Check,
  Send,
} from 'lucide-react';
import { api } from '../../api';
import type { AccountStatus, DeviceRole, SyncStatus } from '../../types/models';

/** Tauri and Axum both reject with a plain string, not an `Error`. */
function errMsg(err: unknown): string {
  return err instanceof Error ? err.message : String(err);
}

/** Small coloured pill for a device's role (mirrors AccountSection's badge). */
function RoleBadge({ role }: { role: DeviceRole | null }) {
  if (role === 'primary') {
    return (
      <span className="inline-flex items-center rounded-full bg-accent/20 px-2 py-0.5 text-xs font-medium text-accent">
        Primary
      </span>
    );
  }
  if (role === 'capture') {
    return (
      <span className="inline-flex items-center rounded-full bg-purple/20 px-2 py-0.5 text-xs font-medium text-purple">
        Capture
      </span>
    );
  }
  return (
    <span className="inline-flex items-center rounded-full bg-surface-hover px-2 py-0.5 text-xs font-medium text-content-muted">
      Not assigned
    </span>
  );
}

export default function SyncSection() {
  const mounted = useRef(true);

  const [status, setStatus] = useState<AccountStatus | null>(null);
  const [loadingStatus, setLoadingStatus] = useState(true);

  const [syncStatus, setSyncStatus] = useState<SyncStatus | null>(null);
  const [autoMode, setAutoMode] = useState(false);
  const [autoSaving, setAutoSaving] = useState(false);
  const [autoError, setAutoError] = useState<string | null>(null);
  const [devFlag, setDevFlag] = useState(false);

  // Dev pairing-ticket disclosure — lazily fetched on first expand.
  const [showTicket, setShowTicket] = useState(false);
  const [ticket, setTicket] = useState<string | null>(null);
  const [ticketError, setTicketError] = useState<string | null>(null);
  const [ticketLoading, setTicketLoading] = useState(false);
  const [copied, setCopied] = useState(false);

  const signedIn = status?.signedIn ?? false;
  const role = status?.role ?? null;
  const isCapture = role === 'capture';

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
        const [ss, auto, devVal] = await Promise.all([
          api.invoke<SyncStatus>('get_sync_status'),
          api.invoke<boolean>('get_sync_auto_mode'),
          api.invoke<string>('get_setting', {
            key: 'sync.dev_ticket_pairing',
            defaultValue: 'false',
          }),
        ]);
        if (!mounted.current) return;
        setSyncStatus(ss ?? null);
        setAutoMode(!!auto);
        setDevFlag(devVal.toLowerCase() === 'true');
      } catch (err) {
        console.error('[sync] load sync settings failed:', err);
      }
    })();
  }, [signedIn]);

  const handleToggleAuto = async () => {
    if (autoSaving || !isCapture) return;
    const next = !autoMode;
    setAutoMode(next); // optimistic
    setAutoSaving(true);
    setAutoError(null);
    try {
      await api.invoke('set_sync_auto_mode', { enabled: next });
    } catch (err) {
      console.error('[sync] set auto mode failed:', err);
      if (mounted.current) {
        setAutoMode(!next); // revert
        setAutoError(errMsg(err));
      }
    } finally {
      if (mounted.current) setAutoSaving(false);
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
    try {
      await navigator.clipboard?.writeText(ticket);
      setCopied(true);
      setTimeout(() => {
        if (mounted.current) setCopied(false);
      }, 2000);
    } catch (err) {
      console.error('[sync] copy ticket failed:', err);
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
      {/* Machine role */}
      <div>
        <h4 className="text-sm font-medium text-content-secondary mb-2">Machine role</h4>
        <div className="flex items-center gap-2 text-sm text-content-muted">
          <Monitor size={14} className="flex-shrink-0" />
          This machine:
          <RoleBadge role={role} />
        </div>
        {role === null && (
          <p className="text-xs text-content-muted mt-2">
            Set this machine&apos;s role in the <span className="text-content-secondary">Account</span>{' '}
            section above. A <strong>Capture</strong> device sends its frames to a paired{' '}
            <strong>Primary</strong>.
          </p>
        )}
      </div>

      {/* Auto-send (capture nodes only) */}
      <div>
        <h4 className="text-sm font-medium text-content-secondary mb-1">Automatic send</h4>
        <label
          className={`flex items-start gap-3 ${isCapture ? 'cursor-pointer' : 'cursor-not-allowed opacity-70'}`}
        >
          <input
            type="checkbox"
            checked={autoMode}
            disabled={!isCapture || autoSaving}
            onChange={handleToggleAuto}
            className="mt-0.5 w-4 h-4 rounded border-border bg-surface-hover text-accent focus:ring-2 focus:ring-accent focus:ring-offset-0 disabled:opacity-50"
          />
          <div>
            <span className="flex items-center gap-2 text-sm font-medium text-content-secondary">
              <Send size={13} className="flex-shrink-0" />
              Send newly scanned frames automatically
              {autoSaving && <Loader2 size={13} className="animate-spin text-content-muted" />}
            </span>
            <p className="text-xs text-content-muted mt-0.5">
              {isCapture
                ? 'When on, frames discovered by a scan (manual or background monitoring) are queued to your paired primary right away.'
                : 'Auto-send applies to Capture devices only. Set this machine to the Capture role in the Account section to enable it.'}
            </p>
          </div>
        </label>
        {autoError && (
          <div className="mt-2 flex items-start gap-2 rounded-lg border border-error/50 bg-error-muted p-2.5">
            <AlertTriangle size={16} className="text-error flex-shrink-0 mt-0.5" />
            <p className="text-sm text-error">{autoError}</p>
          </div>
        )}
      </div>

      {/* Pairing / receiver status */}
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
          {isCapture && (
            <div className="flex items-start gap-2">
              <Send size={14} className="flex-shrink-0 mt-0.5" />
              <span>
                This machine sends its frames to your paired primary. If a send is rejected, re-pair
                in the <span className="text-content-secondary">Account</span> section.
              </span>
            </div>
          )}
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
                      title="Copy ticket"
                      className="flex-shrink-0 inline-flex items-center gap-1 rounded-md border border-border px-2 py-1.5 text-xs text-content-secondary hover:bg-surface-hover transition-colors"
                    >
                      {copied ? <Check size={13} className="text-success" /> : <Copy size={13} />}
                      {copied ? 'Copied' : 'Copy'}
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
