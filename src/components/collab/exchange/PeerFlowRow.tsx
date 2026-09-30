import { useState, type JSX } from 'react';
import { formatBytes, formatDuration, formatRate } from '../format';
import type { FlowView } from '../../../types/models';

/**
 * One live peer's transfer row — receiving or sending, shared by the
 * project's Exchange tab and (Task 17) the Transfers page, so there is one
 * source of truth for "what does a flow look like" (spec 2026-09-30
 * "Exchange", mockup `peerRow`/`spark`).
 *
 * Expandable: the row itself is the toggle (`aria-expanded`), revealing one
 * line per in-flight frame. The sparkline is an inline SVG polyline drawn in
 * `currentColor`, coloured only by its wrapper's token class
 * (`text-accent` recv / `text-success` send) — never a raw colour.
 */

const SPARK_WIDTH = 120;
const SPARK_HEIGHT = 26;

function sparklinePoints(rates: number[]): string {
  if (rates.length === 0) return '';
  const max = Math.max(...rates) * 1.1 || 1;
  return rates
    .map((v, i) => {
      const x = rates.length > 1 ? (i / (rates.length - 1)) * SPARK_WIDTH : 0;
      const y = SPARK_HEIGHT - 2 - (v / max) * (SPARK_HEIGHT - 4);
      return `${x.toFixed(1)},${y.toFixed(1)}`;
    })
    .join(' ');
}

export function PeerFlowRow({
  flow,
  label,
  rates,
  tone = 'bg-surface-hover',
  toneColor,
}: {
  flow: FlowView;
  label: { member: string | null; device: string };
  rates: number[];
  tone?: string;
  /** Hex colour from `memberColor`; overrides the `tone` class when set. */
  toneColor?: string;
}): JSX.Element {
  const [open, setOpen] = useState(false);
  const isRecv = flow.direction === 'recv';
  const who = label.member ?? label.device;
  const initial = who.slice(0, 1).toUpperCase();
  const arrow = isRecv ? '↓' : '↑';
  const verb = isRecv ? 'from' : 'to';
  const completedWord = isRecv ? 'landed' : 'served';
  const toneText = tone === 'bg-surface-hover' && !toneColor ? 'text-content' : 'text-surface';
  const sparkColor = isRecv ? 'text-accent' : 'text-success';
  const points = sparklinePoints(rates);

  return (
    <div>
      <button
        type="button"
        aria-expanded={open}
        onClick={() => setOpen((o) => !o)}
        className="flex w-full items-center gap-3 px-2 py-2 text-left transition-colors hover:bg-surface-hover"
      >
        <span
          className={`flex h-7 w-7 shrink-0 items-center justify-center rounded-full text-xs font-medium ${tone} ${toneText}`}
          style={toneColor ? { backgroundColor: toneColor } : undefined}
        >
          {initial}
        </span>
        <span className="min-w-0 flex-1">
          <span className="block truncate text-sm font-medium text-content">{`${arrow} ${verb} ${who}`}</span>
          <span className="block truncate text-xs text-content-muted">
            {`${label.device} · ${flow.inFlight.length} in flight · ${flow.completed} ${completedWord} this session · ${formatBytes(flow.bytesSession)}`}
          </span>
        </span>
        <span className="w-20 shrink-0 text-right text-sm text-content-secondary">{formatRate(flow.rateBps)}</span>
        <span className={`${sparkColor} shrink-0`}>
          <svg
            width={SPARK_WIDTH}
            height={SPARK_HEIGHT}
            viewBox={`0 0 ${SPARK_WIDTH} ${SPARK_HEIGHT}`}
            aria-hidden="true"
          >
            <polyline points={points} fill="none" stroke="currentColor" strokeWidth="1.4" />
          </svg>
        </span>
        {flow.etaSecs !== null && (
          <span className="w-24 shrink-0 text-right text-xs text-content-muted">
            {`ETA ${formatDuration(flow.etaSecs)}`}
          </span>
        )}
      </button>
      {open && (
        <div className="space-y-1 py-1 pl-12 pr-2">
          {flow.inFlight.map((item) => {
            const pct = item.size > 0 ? Math.min(100, (item.done / item.size) * 100) : 0;
            return (
              <div key={item.frameUuid} className="flex items-center gap-2 text-xs text-content-muted">
                <span className="min-w-0 flex-1 truncate">{item.fileName}</span>
                <span className="h-1.5 w-24 shrink-0 overflow-hidden rounded-full bg-surface-hover">
                  <span
                    className={`block h-full ${isRecv ? 'bg-accent' : 'bg-success'}`}
                    style={{ width: `${pct}%` }}
                  />
                </span>
                <span className="w-32 shrink-0 text-right">
                  {formatBytes(item.done)} / {formatBytes(item.size)}
                </span>
              </div>
            );
          })}
        </div>
      )}
    </div>
  );
}
