import type { ButtonHTMLAttributes, HTMLAttributes, ReactNode } from 'react';

/** Mockup `.pill` — 11.5px on elev, radius 999, padding 1×8. */
export function Pill({ children, dot, as = 'span', className = '', disabled, ...rest }: HTMLAttributes<HTMLElement> & Pick<ButtonHTMLAttributes<HTMLButtonElement>, 'disabled'> & { dot?: ReactNode; as?: 'span' | 'button' }) {
  const cls = `inline-flex items-center gap-[5px] rounded-full bg-surface-elevated px-2 py-px text-[11.5px] leading-[1.4] text-content-muted ${as === 'button' ? 'enabled:hover:bg-surface-hover disabled:cursor-not-allowed disabled:opacity-45' : ''} ${className}`;
  if (as === 'button') return <button type="button" className={cls} disabled={disabled} {...rest}>{dot}{children}</button>;
  return <span className={cls} {...rest}>{dot}{children}</span>;
}
