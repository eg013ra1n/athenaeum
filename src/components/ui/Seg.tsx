import type { ReactNode } from 'react';

/** Mockup `.seg` — bordered segmented control. With `ariaLabel` it is a named
 *  `group`; `disabled` disables every option and dims the strip. */
export function Seg<T extends string>({
  options,
  value,
  onChange,
  ariaLabel,
  disabled = false,
}: {
  options: { value: T; label: ReactNode }[];
  value: T;
  onChange: (v: T) => void;
  ariaLabel?: string;
  disabled?: boolean;
}) {
  return (
    <span
      role={ariaLabel ? 'group' : undefined}
      aria-label={ariaLabel}
      className={`inline-flex overflow-hidden rounded-[5px] border border-border ${disabled ? 'opacity-60' : ''}`}
    >
      {options.map((o) => (
        <button
          key={o.value}
          type="button"
          aria-pressed={o.value === value}
          disabled={disabled}
          onClick={() => onChange(o.value)}
          className={`border-r border-border px-2.5 py-[3px] leading-[1.4] last:border-r-0 ${o.value === value ? 'bg-surface-elevated text-content' : 'text-content-faint'}`}
        >
          {o.label}
        </button>
      ))}
    </span>
  );
}
