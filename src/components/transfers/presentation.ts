// Shared presentation helpers for the Transfers UI (Transfers Status Model v2,
// §D5/§D8). Every surface — the master list rows (`TransferRow`), the detail
// pane, and the slide-over mini-rows — maps the same backend `displayState`
// into the same chip, and formats bytes/speed/ETA/error the same way, from here.
// Keeping this in one module is what makes "device names everywhere, hex only in
// Details, benign waiting not sticky errors" a single source of truth.

/** Human byte size, e.g. `18.7 MB`. Shared by every Transfers surface. */
export function formatBytes(n: number): string {
  if (!isFinite(n) || n < 0) return '—';
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
  if (n < 1024 * 1024 * 1024) return `${(n / 1024 / 1024).toFixed(1)} MB`;
  return `${(n / 1024 / 1024 / 1024).toFixed(2)} GB`;
}

/** Throughput label, e.g. `18.7 MB/s`. `null` when speed is unknown/zero so the
 *  caller can omit it (a stalled/idle row shows no speed). */
export function formatSpeed(bps: number | null): string | null {
  if (bps == null || !isFinite(bps) || bps <= 0) return null;
  return `${formatBytes(bps)}/s`;
}

/** `mm:ss` remaining until an armed retry deadline (`stalledUntil`/`nextRetryAt`),
 *  clamped at zero. Recomputed each 1s tick from a passed-in `now`. */
export function formatCountdown(deadlineIso: string, now: number): string {
  const deadline = new Date(deadlineIso).getTime();
  const remainingMs = Math.max(0, deadline - now);
  const totalSec = Math.ceil(remainingMs / 1000);
  const m = Math.floor(totalSec / 60);
  const s = totalSec % 60;
  return `${m}:${s.toString().padStart(2, '0')}`;
}

/** ETA from remaining bytes + smoothed speed. `∞` when nothing is moving
 *  (speed ≈ 0) — an honest "unestimable", never a fake number. */
export function formatEta(remainingBytes: number, speedBps: number | null): string {
  if (speedBps == null || !isFinite(speedBps) || speedBps <= 0) return '∞';
  const secs = remainingBytes / speedBps;
  if (!isFinite(secs) || secs < 0) return '∞';
  if (secs < 1) return '<1s';
  const s = Math.round(secs);
  if (s < 60) return `${s}s`;
  const m = Math.floor(s / 60);
  if (m < 60) return `${m}m ${s % 60}s`;
  const h = Math.floor(m / 60);
  return `${h}h ${m % 60}m`;
}

/** Leading chars of a node-id hex, enough to disambiguate — the LAST-RESORT
 *  fallback when no friendly device name is known. Names win everywhere except
 *  the Details tab. */
export function shortPeer(hex: string): string {
  const t = hex.trim();
  return t.length > 10 ? t.slice(0, 10) : t;
}

/** Fallback label for a project chip when the id → title map has no entry. */
export function shortProject(id: string): string {
  const t = id.trim();
  return t.length > 8 ? t.slice(0, 8) : t;
}

/**
 * Map a raw sync `last_error` string to a short, human-readable reason. Known
 * strings match as case-insensitive prefixes so trailing context still resolves;
 * anything unrecognized falls through verbatim so an error is never hidden. Keep
 * the raw string on a `title=` hover at the call site. (Moved verbatim from the
 * former `ActiveTransferRow` so both the list and the slide-over share it.)
 *
 * `terminal` (UX wave 2): on a SETTLED row a "— will keep retrying" promise is a
 * lie (nothing is retrying), so the transient-family causes drop that suffix and
 * render as the plain cause (e.g. «Peer didn't answer»). Explicit terminal causes
 * (cancelled-by-peer, payload-missing, refused, unknown reasons) are unaffected.
 * Non-terminal callers keep the full "will keep retrying" wording (default).
 */
export function plainTransferError(raw: string, terminal = false): string {
  const s = raw.trim().toLowerCase();
  // The retry promise only holds while a retry is genuinely pending.
  const retryHint = terminal ? '' : ' — will keep retrying';
  if (s.startsWith('no ack from peer within timeout')) return `Peer didn't respond${retryHint}`;
  if (s.startsWith('package payload missing on disk')) return 'Local package data is missing';
  if (s.startsWith('cancelled by receiver')) return 'Cancelled by the receiving device';
  // Class-prefixed dial failures the sync engine persists as `<class>: <raw>`.
  // A retryable class is a warning, not a failure — delivery-forever keeps
  // trying — so the copy says so (until the row settles, per `terminal`).
  // `other:` sheds its machine prefix; any unknown prefix falls through verbatim
  // (the raw string still rides the `title=` hover).
  if (s.startsWith('no_route:')) return `No route to peer${retryHint}`;
  if (s.startsWith('relay_unreachable:')) return `Peer unreachable via relay${retryHint}`;
  if (s.startsWith('refused:')) return 'Peer refused the connection';
  if (s.startsWith('timeout:')) return `Peer didn't answer${retryHint}`;
  if (s.startsWith('not_started:')) return `Peer app not running${retryHint}`;
  if (s.startsWith('other:')) return raw.slice(raw.indexOf(':') + 1).trim() || raw;
  return raw;
}

/** Whether a raw `last_error` belongs to the transient "— will keep retrying"
 *  family (a dial/ack cause, not a hard verdict). On a TERMINAL row these render
 *  in a MUTED tone (the retry promise is void), unlike explicit terminal causes
 *  (cancelled-by-peer, payload-missing, refused, unknown failures) which keep
 *  their error styling. Mirrors the prefix set in [`plainTransferError`]. */
export function isTransientTransferError(raw: string): boolean {
  const s = raw.trim().toLowerCase();
  return (
    s.startsWith('no ack from peer within timeout') ||
    s.startsWith('no_route:') ||
    s.startsWith('relay_unreachable:') ||
    s.startsWith('timeout:') ||
    s.startsWith('not_started:')
  );
}

/** A rendered state chip: a short label + its design-token tone classes. */
export interface StateChip {
  label: string;
  /** Background + text token classes (never raw hex). */
  className: string;
}

const CHIP_MUTED = 'bg-surface-hover text-content-muted';
const CHIP_NEUTRAL = 'bg-surface-hover text-content-secondary';
const CHIP_ACCENT = 'bg-accent/15 text-accent';
const CHIP_SUCCESS = 'bg-success/15 text-success';
const CHIP_ERROR = 'bg-error/15 text-error';

/**
 * The backend-derived `displayState` → chip (§D5/§D8). `waiting` is NEUTRAL
 * (never red, never error) — a benign "wants to move, nothing right now". The
 * only red chip is the terminal `failed`. `uploaded` is accent with the
 * "awaiting confirmation" subline (see [`displayStateSubline`]).
 */
export function displayStateChip(displayState: string): StateChip {
  switch (displayState) {
    case 'queued':
      return { label: 'queued', className: CHIP_MUTED };
    case 'preparing':
      return { label: 'preparing', className: CHIP_MUTED };
    case 'announced':
      return { label: 'announced', className: CHIP_MUTED };
    case 'transferring':
      return { label: 'transferring', className: CHIP_ACCENT };
    case 'fetching':
      return { label: 'fetching', className: CHIP_ACCENT };
    case 'ingesting':
      return { label: 'ingesting', className: CHIP_ACCENT };
    case 'uploaded':
      return { label: 'uploaded', className: CHIP_ACCENT };
    case 'waiting':
      return { label: 'waiting', className: CHIP_NEUTRAL };
    case 'confirmed':
      return { label: 'confirmed', className: CHIP_SUCCESS };
    case 'done':
      return { label: 'done', className: CHIP_SUCCESS };
    case 'cancelled':
      return { label: 'cancelled', className: CHIP_MUTED };
    case 'failed':
      return { label: 'failed', className: CHIP_ERROR };
    default:
      return { label: displayState, className: CHIP_MUTED };
  }
}

/** The muted subline shown under a chip for states that need a plain-English
 *  qualifier — currently only `uploaded → "awaiting confirmation"` (§D5: the
 *  provider finished serving, the receiver ack hasn't landed). */
export function displayStateSubline(displayState: string): string | null {
  if (displayState === 'uploaded') return 'awaiting confirmation';
  return null;
}

/** Per-file / per-frame outcome chip tone (Files tab + history rows). */
export function outcomeChipClass(outcome: string): string {
  if (outcome === 'ingested') return CHIP_SUCCESS;
  if (outcome === 'duplicate') return 'bg-warning/15 text-warning';
  if (outcome.startsWith('rejected') || outcome.startsWith('failed')) return CHIP_ERROR;
  if (outcome === 'cancelled') return CHIP_MUTED;
  if (outcome === 'uploaded') return CHIP_ACCENT;
  return CHIP_MUTED;
}

/** Per-file lifecycle-state chip tone (a file that has no settled outcome yet). */
export function fileStateChipClass(state: string): string {
  switch (state) {
    case 'done':
    case 'uploaded':
      return CHIP_ACCENT;
    case 'sending':
    case 'fetching':
      return CHIP_ACCENT;
    case 'failed':
      return CHIP_ERROR;
    default: // pending / announced
      return CHIP_MUTED;
  }
}

/** Humanize a snake_case `sync_events` kind (§D7) into a short phrase for the
 *  Log tab. Unknown kinds are de-underscored so a new backend kind still reads. */
export function humanizeEventKind(kind: string): string {
  const map: Record<string, string> = {
    enqueued: 'Queued for send',
    negotiated: 'Dedup handshake done',
    announce_sent: 'Announce sent',
    announce_failed: 'Announce failed',
    serve_started: 'Serving started',
    uploaded: 'Uploaded',
    ack_received: 'Ack received',
    confirmed: 'Delivery confirmed',
    ack_timeout: 'Ack timed out',
    retry_scheduled: 'Retry scheduled',
    cancelled: 'Cancelled',
    failed: 'Failed',
    announce_received: 'Announce received',
    fetch_started: 'Fetch started',
    fetch_failed: 'Fetch failed',
    ingest_started: 'Ingest started',
    ingested: 'Ingested',
    replayed: 'Replayed (already received)',
    dial_failed: 'Dial failed',
  };
  return map[kind] ?? kind.replace(/_/g, ' ');
}
