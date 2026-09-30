/** Mockup `.segs button` — My-frames segment tiles (number 18px, label 12px). */
const TONE = { accent: 'text-accent', success: 'text-success', warning: 'text-warning', content: 'text-content' } as const;
export function SegmentTiles<T extends string>({ tiles, value, onChange }: {
  tiles: { value: T; n: number | string; label: string; tone: keyof typeof TONE }[];
  value: T;
  onChange: (v: T) => void;
}) {
  return (
    <div className="flex flex-wrap gap-2">
      {tiles.map((t) => {
        const on = t.value === value;
        return (
          <button
            key={t.value}
            type="button"
            aria-pressed={on}
            onClick={() => onChange(t.value)}
            className={`flex min-w-[150px] flex-col items-start gap-px rounded-md border px-3.5 py-[7px] text-left leading-[1.4] ${on ? 'border-accent bg-accent/[0.08]' : 'border-border hover:bg-surface-hover'}`}
          >
            <span className={`text-[18px] font-semibold ${TONE[t.tone]}`}>{typeof t.n === 'number' ? t.n.toLocaleString('en-US') : t.n}</span>{' '}
            <span className={`text-[12px] ${on ? 'text-accent' : 'text-content-faint'}`}>{t.label}</span>
          </button>
        );
      })}
    </div>
  );
}
