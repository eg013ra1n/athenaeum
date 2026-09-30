import { useEffect, useRef, useState } from 'react';
import { Loader2, RefreshCw, Wifi, WifiOff } from 'lucide-react';
import { api } from '../../api';
import { useNotifications } from '../../contexts/NotificationContext';
import { useDeviceReplace } from './DeviceReplaceDialog';
import { Button, Pill, StatusDot } from '../ui';
import type { CollabLiveStatus as Status } from '../../types/models';

/**
 * The live-exchange status line (spec §14, L10): "Live", "Reconnecting in N s",
 * "Hub unreachable — retrying", the storage state, and "Sync now" — the one
 * global command that reconnects the event channel, clears every back-off and
 * reconciles at once. Follows `collab-live-status`; never reads or classifies
 * the storage itself (that is `DeviceReplaceDialog`'s job, through the
 * "Replace a device…" link).
 *
 * `variant="pill"` (the project page header, spec 2026-09-30 §8) folds all of
 * it into ONE pill: a status dot, then the label ("Live · synced N s ago"
 * while live with storage available); clicking the pill IS Sync now.
 */

/** Storage reasons (`CollabLiveStatus.storageReason`, core's stable
 *  snake_case vocabulary) in the status line's words. */
const REASON: Record<string, string> = {
  path_missing: 'the Collaboration folder is missing',
  not_a_directory: 'the Collaboration folder is missing',
  marker_missing: 'the Collaboration folder is missing',
  marker_mismatch: 'another disk is mounted there',
  other_device: 'this folder belongs to another device',
};

/** The ONE label function for the live status (pure). `elapsedSecs` counts
 *  down a reconnect from the event's `retryInSecs`. */
export function liveStatusLabel(s: Status, elapsedSecs: number): string {
  switch (s.state) {
    case 'live':
      if (s.storage === 'unavailable') {
        const reason = s.storageReason ? (REASON[s.storageReason] ?? s.storageReason) : null;
        return reason ? `Online · storage unavailable (${reason})` : 'Online · storage unavailable';
      }
      if (s.storage === 'readOnly') return 'Online · read-only storage (serving, not downloading)';
      return 'Live';
    case 'connecting':
      return 'Connecting…';
    case 'reconnecting': {
      // At zero the attempt is in flight; a slow one must not read "in 0 s".
      const left = (s.retryInSecs ?? 0) - elapsedSecs;
      return left > 0 ? `Reconnecting in ${left} s` : 'Reconnecting…';
    }
    case 'unreachable':
      return 'Hub unreachable — retrying';
    case 'signedOut':
      return 'Signed out';
    case 'outdated':
      return 'Update required';
    case 'off':
    default:
      return 'Collaboration is off';
  }
}

const PERIODIC_ONLY = 'Changes are seen by periodic check only';

/** The pill's dot (pure): live, a read-only store warns, an unavailable store
 *  or an unreachable hub is an error, everything else is offline. */
export function dotState(s: Status): 'live' | 'warn' | 'error' | 'offline' {
  if (s.state === 'live') return s.storage === 'unavailable' ? 'error' : s.storage === 'readOnly' ? 'warn' : 'live';
  return s.state === 'unreachable' ? 'error' : 'offline';
}

/** A sync time in epoch ms. Core stamps `fetched_at = datetime('now')`:
 *  UTC with no zone designator (`YYYY-MM-DD HH:MM:SS`), which `Date.parse`
 *  would read as LOCAL time — off by the machine's UTC offset. A zone-less
 *  date-time is therefore read as UTC; a string carrying `Z` or `±hh:mm`
 *  keeps its own zone. `NaN` when unparsable. */
function parseSyncedAt(syncedAt: string): number {
  const iso = syncedAt.trim().replace(' ', 'T');
  const zoned = /(?:Z|[+-]\d{2}(?::?\d{2})?)$/i.test(iso);
  return Date.parse(zoned || !iso.includes('T') ? iso : `${iso}Z`);
}

/** Whole units, rounded down: "N s" under 60 s, "N m" under 60 min, "N h"
 *  under 48 h, else "N d". */
function formatAge(secs: number): string {
  if (secs < 60) return `${secs} s`;
  if (secs < 3600) return `${Math.floor(secs / 60)} m`;
  if (secs < 48 * 3600) return `${Math.floor(secs / 3600)} h`;
  return `${Math.floor(secs / 86400)} d`;
}

/** The pill's label (pure): "Live · synced N s ago" (then m / h / d) while
 *  live with storage available and a known sync time, else the one live
 *  status label. An unparsable `syncedAt` reads as unknown — never "NaN s". */
export function pillLabel(s: Status, elapsed: number, syncedAt: string | null, now: number): string {
  const synced = syncedAt ? parseSyncedAt(syncedAt) : NaN;
  if (s.state === 'live' && s.storage === 'available' && Number.isFinite(synced)) {
    const secs = Math.max(0, Math.round((now - synced) / 1000));
    return `Live · synced ${formatAge(secs)} ago`;
  }
  return liveStatusLabel(s, elapsed);
}

function toneOf(s: Status): string {
  if (s.state === 'live') {
    if (s.storage === 'unavailable') return 'text-error';
    if (s.storage === 'readOnly') return 'text-warning';
    return 'text-success';
  }
  if (s.state === 'unreachable') return 'text-error';
  if (s.state === 'reconnecting' || s.state === 'connecting' || s.state === 'outdated') return 'text-warning';
  return 'text-content-muted';
}

export default function CollabLiveStatus({
  compact = false,
  variant = 'default',
  syncedAt = null,
  onSynced,
}: {
  compact?: boolean;
  variant?: 'default' | 'pill';
  /** The pill's "synced N s ago" origin (the project card's `fetchedAt`). */
  syncedAt?: string | null;
  /** Called after `collab_sync_now` succeeds (never on failure) — the project
   *  page re-reads its card, so the synced age restarts. */
  onSynced?: () => void;
}) {
  const { notify } = useNotifications();
  const { available: canReplace, pending, requestOpen } = useDeviceReplace();
  const [status, setStatus] = useState<Status | null>(null);
  const [elapsed, setElapsed] = useState(0);
  const [syncing, setSyncing] = useState(false);
  const [now, setNow] = useState(() => Date.now());
  // An event that arrived before the initial read resolves is newer: keep it.
  const gotEvent = useRef(false);

  useEffect(() => {
    let cancelled = false;
    api
      .invoke<Status>('get_collab_live_status')
      .then((s) => {
        if (!cancelled && !gotEvent.current) setStatus(s);
      })
      .catch((err) => console.error('[collab] get_collab_live_status failed:', err));
    return () => {
      cancelled = true;
    };
  }, []);

  useEffect(() => {
    let cancelled = false;
    let unlisten: (() => void) | undefined;
    api
      .listen<Status>('collab-live-status', (s) => {
        if (cancelled) return;
        gotEvent.current = true;
        setStatus(s);
        setElapsed(0);
      })
      .then((fn) => {
        if (cancelled) fn();
        else unlisten = fn;
      })
      .catch((err) => console.error('[collab] live-status listen failed:', err));
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, []);

  // The reconnect countdown — a display tick, not a poll.
  const reconnecting = status?.state === 'reconnecting';
  useEffect(() => {
    if (!reconnecting) return;
    const t = setInterval(() => setElapsed((e) => e + 1), 1000);
    return () => clearInterval(t);
  }, [reconnecting, status?.since, status?.retryInSecs]);

  // The pill's "synced N s ago" — a display tick, not a poll.
  useEffect(() => {
    if (variant !== 'pill') return;
    const t = setInterval(() => setNow(Date.now()), 1000);
    return () => clearInterval(t);
  }, [variant]);

  const syncNow = async () => {
    setSyncing(true);
    try {
      await api.invoke('collab_sync_now');
      onSynced?.();
    } catch (err) {
      console.error('[collab] collab_sync_now failed:', err);
      notify({
        title: 'Sync now failed',
        detail: err instanceof Error ? err.message : String(err),
        kind: 'project',
        tone: 'warning',
        hasErrors: true,
      });
    } finally {
      setSyncing(false);
    }
  };

  if (!status) return null;
  const live = status.state === 'live';
  const periodicOnly = status.watcherDegraded || status.networkVolume;
  const showOwnerLink = canReplace && (pending !== null || status.storageReason === 'other_device');
  const off = status.state === 'off';

  if (variant === 'pill') {
    return (
      <span className="inline-flex items-center gap-2">
        <Pill
          as="button"
          className="whitespace-nowrap"
          onClick={() => void syncNow()}
          disabled={syncing || off}
          title={[
            periodicOnly ? PERIODIC_ONLY : null,
            off
              ? 'Collaboration is off — sign in and set a Collaboration folder first'
              : 'Sync now: reconnect to the hub and check every project',
          ]
            .filter(Boolean)
            .join(' · ')}
          dot={<StatusDot state={dotState(status)} />}
        >
          {syncing ? 'Syncing…' : pillLabel(status, elapsed, syncedAt, now)}
        </Pill>
        {showOwnerLink && (
          <Button variant="link" size="sm" onClick={requestOpen}>
            {pending === 'replace' ? 'Replace a device…' : 'Resolve the folder owner…'}
          </Button>
        )}
      </span>
    );
  }

  return (
    <div className="flex flex-wrap items-center gap-x-3 gap-y-1 text-sm">
      <span
        className={`flex items-center gap-1.5 ${toneOf(status)}`}
        title={compact && periodicOnly ? PERIODIC_ONLY : undefined}
      >
        {live ? <Wifi size={14} className="shrink-0" /> : <WifiOff size={14} className="shrink-0" />}
        {liveStatusLabel(status, elapsed)}
      </span>
      {periodicOnly && !compact && <span className="text-xs text-content-muted">{PERIODIC_ONLY}</span>}
      {showOwnerLink && (
        <button
          type="button"
          onClick={requestOpen}
          className="text-xs text-accent underline-offset-2 hover:underline"
        >
          {pending === 'replace' ? 'Replace a device…' : 'Resolve the folder owner…'}
        </button>
      )}
      <button
        type="button"
        onClick={() => void syncNow()}
        disabled={syncing || off}
        title={
          off
            ? 'Collaboration is off — sign in and set a Collaboration folder first'
            : 'Reconnect to the hub and check every project at once'
        }
        className="inline-flex items-center gap-1.5 rounded border border-border px-2.5 py-1 text-xs text-content-secondary transition-colors hover:bg-surface-hover disabled:cursor-not-allowed disabled:opacity-50"
      >
        {syncing ? <Loader2 size={12} className="animate-spin" /> : <RefreshCw size={12} />}
        Sync now
      </button>
    </div>
  );
}
