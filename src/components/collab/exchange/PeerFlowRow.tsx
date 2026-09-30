import { useState, type JSX } from 'react';
import { formatDurationPadded, formatRate, formatSize } from '../format';
import { MEMBER_PALETTE } from '../project/memberColors';
import { ProgressBar } from '../../ui/Bars';
import { Sparkline } from '../../ui/Sparkline';
import type { FlowView } from '../../../types/models';

/**
 * One live peer's transfer row — receiving or sending, shared by the
 * project's Exchange tab and the Transfers page, so there is one source of
 * truth for "what does a flow look like" (mockup `peerRow`).
 *
 * Expandable: the row itself is the toggle (`aria-expanded`), revealing one
 * line per in-flight frame. Returns a fragment so rows are flat siblings
 * (`first:border-t-0` on the button works).
 *
 * `color` is a hex member colour for the avatar; absent, the avatar falls
 * back to `bg-surface-hover` with `text-content`.
 */
export function PeerFlowRow({
  flow,
  label,
  rates,
  color,
}: {
  flow: FlowView;
  label: { member: string | null; device: string };
  rates: number[];
  color?: string;
}): JSX.Element {
  const [open, setOpen] = useState(false);
  const isRecv = flow.direction === 'recv';
  const who = label.member ?? label.device;
  const initial = who.slice(0, 1).toUpperCase();
  const totalSize = flow.inFlight.reduce((a, f) => a + f.size, 0);
  const totalDone = flow.inFlight.reduce((a, f) => a + f.done, 0);
  const inflightPct = totalSize > 0 ? (100 * totalDone) / totalSize : 0;
  const barColor = isRecv ? undefined : MEMBER_PALETTE[1];

  return (
    <>
      <button
        type="button"
        aria-expanded={open}
        onClick={() => setOpen((o) => !o)}
        className="grid w-full grid-cols-[28px_minmax(140px,1.2fr)_minmax(160px,2fr)_90px_128px_70px] items-center gap-2.5 border-t border-line px-1 py-[9px] text-left first:border-t-0 hover:bg-[rgba(67,76,94,0.3)] max-[640px]:grid-cols-[28px_1fr_80px]"
      >
        <span
          className={`grid h-[26px] w-[26px] place-items-center rounded-full text-[11px] font-bold ${color ? 'text-surface' : 'bg-surface-hover text-content'}`}
          style={color ? { backgroundColor: color } : undefined}
        >
          {initial}
        </span>
        <span className="min-w-0">
          <b className="block truncate font-semibold text-content">
            {isRecv ? '↓ from' : '↑ to'} {who}
          </b>
          <small className="block truncate text-[11.5px] text-content-faint">
            {label.device} · {flow.inFlight.length} in flight
          </small>
        </span>
        <span className="max-[640px]:hidden">
          <ProgressBar percent={inflightPct} color={barColor} />
          <small className="text-[11px] text-content-faint">
            {flow.completed} {isRecv ? 'landed' : 'served'} this session · {formatSize(flow.bytesSession)}
          </small>
        </span>
        <span className="text-right font-semibold text-content">{formatRate(flow.rateBps)}</span>
        <span className="max-[640px]:hidden">
          <Sparkline values={rates} color={isRecv ? MEMBER_PALETTE[0] : MEMBER_PALETTE[1]} />
        </span>
        <span className="text-right text-[12px] text-content-faint max-[640px]:hidden">
          ETA {flow.etaSecs === null ? '—' : formatDurationPadded(flow.etaSecs)}
        </span>
      </button>
      {open && (
        <div className="grid gap-[5px] pb-2 pl-[42px] pt-0.5">
          {flow.inFlight.map((item) => (
            <div
              key={item.frameUuid}
              className="grid grid-cols-[minmax(0,1fr)_160px_80px] items-center gap-2.5 text-[12px] text-content-muted"
            >
              <span className="truncate font-mono">{item.fileName}</span>
              <ProgressBar percent={item.size > 0 ? (100 * item.done) / item.size : 0} color={barColor} />
              <span className="text-right">
                {formatSize(item.done)} / {formatSize(item.size)}
              </span>
            </div>
          ))}
        </div>
      )}
    </>
  );
}
