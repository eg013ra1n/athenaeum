import { getFilterColor } from '../../utils/filterColors';

/** Mockup `.fdot` — 8px, radius 2, 5px right margin. */
export function FilterDot({ filter }: { filter: string }) {
  return <span aria-hidden className="mr-[5px] inline-block h-2 w-2 shrink-0 rounded-[2px] align-[0px]" style={{ backgroundColor: getFilterColor(filter) }} />;
}
/** Mockup `.dot` in a member's colour. */
export function MemberDot({ color }: { color: string }) {
  return <span aria-hidden className="inline-block h-[7px] w-[7px] shrink-0 rounded-full" style={{ backgroundColor: color }} />;
}
const STATE = {
  online: 'bg-success',
  offline: 'bg-border',
  live: 'bg-success shadow-[0_0_0_3px_rgba(163,190,140,0.18)]',
  warn: 'bg-warning',
  error: 'bg-error',
} as const;
/** Mockup `.dot` / `.live-dot`. */
export function StatusDot({ state }: { state: keyof typeof STATE }) {
  return <span aria-hidden className={`inline-block h-[7px] w-[7px] shrink-0 rounded-full ${STATE[state]}`} />;
}
