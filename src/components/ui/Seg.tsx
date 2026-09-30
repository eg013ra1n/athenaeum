import type { ReactNode } from 'react';

/** Mockup `.seg` — bordered segmented control. */
export function Seg<T extends string>({ options, value, onChange }: { options: { value: T; label: ReactNode }[]; value: T; onChange: (v: T) => void }) {
  return (
    <span className="inline-flex overflow-hidden rounded-[5px] border border-border">
      {options.map((o) => (
        <button
          key={o.value}
          type="button"
          aria-pressed={o.value === value}
          onClick={() => onChange(o.value)}
          className={`border-r border-border px-2.5 py-[3px] leading-[1.4] last:border-r-0 ${o.value === value ? 'bg-surface-elevated text-content' : 'text-content-faint'}`}
        >
          {o.label}
        </button>
      ))}
    </span>
  );
}
