import { FilterDot } from './Dots';

/** Mockup `.fchip` (+ `.k` count, `.on`, `.zero`). `dot={false}` drops the filter dot, for a chip whose label is not
 *  a filter (the Stacking Frames status chips). */
export function FilterChip({ filter, count, on, onClick, dot = true }: {
  filter: string;
  count: number;
  on: boolean;
  onClick: () => void;
  dot?: boolean;
}) {
  return (
    <button
      type="button"
      aria-pressed={on}
      onClick={onClick}
      className={`inline-flex items-center gap-[5px] rounded-full border px-2 py-0.5 text-[12px] leading-[1.4] ${on ? 'border-accent bg-accent/[0.12] text-content' : 'border-border text-content-muted'} ${count === 0 ? 'opacity-40' : ''}`}
    >
      {dot && <FilterDot filter={filter} />}
      {filter} <span className="text-[10.5px] text-content-faint">{count}</span>
    </button>
  );
}
