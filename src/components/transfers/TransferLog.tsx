import { formatTimestamp } from '../../utils/dateFormatting';
import { humanizeEventKind } from './presentation';
import type { TransferEventEntry } from '../../types/models';

interface TransferLogProps {
  events: TransferEventEntry[];
  loading: boolean;
  /** History groups without a live row id have no journal — show a quiet note. */
  unavailable?: boolean;
}

/**
 * Detail-pane Log tab (§D7) — the batch's `sync_events` journal, newest-first.
 * A connection hiccup is a timestamped line here, NOT a permanent status on the
 * list row (that separation is the whole point of state ⊥ error).
 */
export function TransferLog({ events, loading, unavailable }: TransferLogProps) {
  if (unavailable) {
    return (
      <p className="px-1 py-3 text-xs text-content-muted">
        The event log is available for recent transfers only.
      </p>
    );
  }
  if (loading && events.length === 0) {
    return <p className="px-1 py-3 text-xs text-content-muted">Loading log…</p>;
  }
  if (events.length === 0) {
    return <p className="px-1 py-3 text-xs text-content-muted">No events recorded yet.</p>;
  }
  return (
    <ul className="space-y-1 py-1">
      {events.map((e, i) => (
        <li key={`${e.ts}-${e.kind}-${i}`} className="flex items-start gap-3 text-xs">
          <span className="shrink-0 text-content-muted tabular-nums">{formatTimestamp(e.ts)}</span>
          <span className="shrink-0 font-medium text-content-secondary">{humanizeEventKind(e.kind)}</span>
          {e.detail && <span className="min-w-0 flex-1 truncate text-content-muted" title={e.detail}>{e.detail}</span>}
        </li>
      ))}
    </ul>
  );
}
