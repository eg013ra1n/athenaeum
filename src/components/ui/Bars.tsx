/** Mockup `.bar` — 8px stacked bar on surface-hover. */
export function Bar({ segments, total, className = '' }: { segments: { value: number; color?: string; className?: string; title?: string }[]; total?: number; className?: string }) {
  const t = total ?? segments.reduce((a, s) => a + s.value, 0);
  return (
    <span className={`flex h-2 w-full overflow-hidden rounded-[2px] bg-surface-hover ${className}`}>
      {t > 0 && segments.filter((s) => s.value > 0).map((s, i) => (
        <i
          key={i}
          title={s.title}
          className={`block h-full ${s.className ?? ''}`}
          style={{ width: `${(s.value / t) * 100}%`, ...(s.color ? { backgroundColor: s.color } : {}) }}
        />
      ))}
    </span>
  );
}
/** Mockup `.pbar` — 4px progress. */
export function ProgressBar({ percent, color, className = '' }: { percent: number; color?: string; className?: string }) {
  const p = Math.max(0, Math.min(100, Number.isFinite(percent) ? percent : 0));
  return (
    <span className={`block h-1 overflow-hidden rounded-[2px] bg-surface-hover ${className}`}>
      <i className={`block h-full ${color ? '' : 'bg-accent'}`} style={{ width: `${p}%`, ...(color ? { backgroundColor: color } : {}) }} />
    </span>
  );
}
