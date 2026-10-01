import type { ButtonHTMLAttributes } from 'react';

/** Mockup `.btn` / `.btn.pri` / `.btn.sm` / `.btn[disabled]` / `.linkbtn`. */
export type ButtonVariant = 'default' | 'primary' | 'danger' | 'dangerPrimary' | 'link';
export interface ButtonProps extends ButtonHTMLAttributes<HTMLButtonElement> {
  variant?: ButtonVariant;
  /** `tile`: the box of the mockup's segment-tile row (33 px tall, min 150,
   *  7×14 padding, radius 6), text left-aligned 15 px in (R34). */
  size?: 'md' | 'sm' | 'tile';
}
const SIZE = {
  md: 'rounded px-2.5 py-1 text-[12px]',
  sm: 'rounded px-[7px] py-px text-[11px]',
  tile: 'h-[33px] min-w-[150px] rounded-md px-3.5 py-[7px] text-[12px]',
} as const;
const VARIANT: Record<Exclude<ButtonVariant, 'link'>, string> = {
  default: 'border-border text-content-secondary hover:bg-surface-hover',
  primary: 'border-accent bg-accent font-semibold text-surface hover:border-accent-hover hover:bg-accent-hover',
  danger: 'border-error/60 text-error hover:bg-error/10',
  dangerPrimary: 'border-error bg-error font-semibold text-surface hover:brightness-110',
};
export function Button({ variant = 'default', size = 'md', className = '', type = 'button', ...rest }: ButtonProps) {
  const cls =
    // `.linkbtn` never reset the UA button padding, so the mockup's links render with 1px 6px.
    variant === 'link'
      ? `px-1.5 py-px leading-[1.4] text-accent hover:underline disabled:cursor-not-allowed disabled:opacity-45 disabled:no-underline ${size === 'sm' ? 'text-[11px]' : 'text-[12px]'}`
      : `inline-flex items-center gap-1.5 whitespace-nowrap border leading-[1.4] transition-colors disabled:cursor-not-allowed disabled:opacity-45 ${SIZE[size]} ${VARIANT[variant]}`;
  return <button type={type} className={`${cls} ${className}`} {...rest} />;
}
