import { getFilterColor } from '../../utils/filterColors';

/** Mockup `.fdot` — 8px, radius 2, 5px right margin. */
export function FilterDot({ filter }: { filter: string }) {
  return <span aria-hidden className="mr-[5px] inline-block h-2 w-2 shrink-0 rounded-[2px] align-[0px]" style={{ backgroundColor: getFilterColor(filter) }} />;
}
/** Mockup `.dot` in a member's colour; an unknown member (no colour) is the
 *  neutral `border` dot, never the accent. */
export function MemberDot({ color }: { color?: string }) {
  return color
    ? <span aria-hidden className="inline-block h-[7px] w-[7px] shrink-0 rounded-full" style={{ backgroundColor: color }} />
    : <span aria-hidden className="inline-block h-[7px] w-[7px] shrink-0 rounded-full bg-border" />;
}
const STATE = {
  online: 'bg-success',
  offline: 'bg-border',
  live: 'bg-success ring-[3px] ring-success/[0.18]',
  warn: 'bg-warning',
  error: 'bg-error',
} as const;
/** Mockup `.dot` / `.live-dot`. */
export function StatusDot({ state }: { state: keyof typeof STATE }) {
  return <span aria-hidden className={`inline-block h-[7px] w-[7px] shrink-0 rounded-full ${STATE[state]}`} />;
}
