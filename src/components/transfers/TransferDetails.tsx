import { formatTimestamp } from '../../utils/dateFormatting';
import { formatBytes, shortPeer } from './presentation';
import type { UnifiedRow } from './types';

/** The `Sender` detail line (Perseus UI v2) — `<device name or short hex> ·
 *  Perseus agent` / `· Athenaeum` for a RECEIVED transfer whose sender kind is
 *  known. Returns `null` (line omitted entirely) for an unknown kind or a sent
 *  transfer, matching the received-row badge's "common case stays clean" rule. */
function senderRow(deviceLabel: string, kind: string | null): { label: string; value: string } | null {
  const kindLabel = kind === 'perseus' ? 'Perseus agent' : kind === 'athenaeum' ? 'Athenaeum' : null;
  if (!kindLabel) return null;
  return { label: 'Sender', value: `${deviceLabel} · ${kindLabel}` };
}

/**
 * Detail-pane Details tab (§D8) — the raw identifiers a batch name/device name
 * hides everywhere else: package uuid/short handle, the peer NODE-ID HEX (this
 * is the ONLY place hex is shown), direction, timings, attempts, raw state, and
 * byte size. Diagnostic surface, not the everyday view.
 */
export function TransferDetails({ item }: { item: UnifiedRow }) {
  const rows: Array<{ label: string; value: string; mono?: boolean }> =
    item.kind === 'live' ? liveRows(item) : historyRows(item);

  return (
    <dl className="grid grid-cols-[9rem_1fr] gap-x-3 gap-y-1.5 py-1 text-xs">
      {rows.map((r) => (
        <div key={r.label} className="contents">
          <dt className="text-content-muted">{r.label}</dt>
          <dd
            className={`min-w-0 break-all text-content-secondary ${r.mono ? 'font-mono text-[11px]' : ''}`}
          >
            {r.value}
          </dd>
        </div>
      ))}
    </dl>
  );
}

function liveRows(item: Extract<UnifiedRow, { kind: 'live' }>): Array<{ label: string; value: string; mono?: boolean }> {
  const r = item.row;
  // Sender kind (Perseus UI v2) — inbound rows only; `null` (line omitted) when unknown.
  const sender = r.kind === 'inbound' ? senderRow(r.deviceName ?? r.peerShort, r.peerKind) : null;
  const rows: Array<{ label: string; value: string; mono?: boolean }> = [
    { label: 'Direction', value: r.kind === 'outbound' ? 'Sending' : 'Receiving' },
    { label: 'Batch name', value: r.displayName ?? '—' },
    { label: 'Device name', value: r.deviceName ?? '—' },
    ...(sender ? [sender] : []),
    { label: 'Package handle', value: r.packageShort, mono: true },
  ];
  if (r.packageId) rows.push({ label: 'Package id', value: r.packageId, mono: true });
  rows.push(
    { label: 'Peer (node id)', value: r.peerShort, mono: true },
    { label: 'Raw state', value: r.state, mono: true },
    { label: 'Display state', value: r.displayState, mono: true },
    { label: 'Created', value: formatTimestamp(r.createdAt) },
    // §D5: `generation` is the user-facing "attempt N" (bumped only by a resend);
    // outbound `attempts` is the engine's internal announce-retry counter.
    { label: 'Attempt (generation)', value: String(r.generation) },
  );
  if (r.kind === 'outbound') rows.push({ label: 'Announce retries', value: String(r.attempts) });
  rows.push({ label: 'Size', value: formatBytes(r.byteSize) });
  if (r.lastError) rows.push({ label: 'Last error', value: r.lastError });
  return rows;
}

function historyRows(
  item: Extract<UnifiedRow, { kind: 'history' }>,
): Array<{ label: string; value: string; mono?: boolean }> {
  const g = item.group;
  // Sender kind (Perseus UI v2) — received groups only; `null` (line omitted) when unknown.
  const sender =
    g.direction === 'received'
      ? senderRow(item.deviceName ?? shortPeer(g.peerDevice), item.deviceKind)
      : null;
  const rows: Array<{ label: string; value: string; mono?: boolean }> = [
    { label: 'Direction', value: g.direction === 'sent' ? 'Sent' : 'Received' },
    { label: 'Batch name', value: g.batchName ?? '—' },
    { label: 'Device name', value: item.deviceName ?? '—' },
    ...(sender ? [sender] : []),
    { label: 'Package id', value: g.packageId ?? '— (legacy)', mono: true },
    { label: 'Peer (node id)', value: g.peerDevice, mono: true },
  ];
  if (g.project) rows.push({ label: 'Project', value: item.projectName ?? g.project, mono: !item.projectName });
  rows.push(
    { label: 'Started', value: formatTimestamp(g.startedAt) },
    { label: 'Finished', value: g.finishedAt ? formatTimestamp(g.finishedAt) : 'in flight' },
    { label: 'Files', value: String(g.rows.length) },
    { label: 'Size', value: formatBytes(g.totalBytes) },
  );
  return rows;
}
