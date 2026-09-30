import { forwardRef, type InputHTMLAttributes, type SelectHTMLAttributes } from 'react';

/** Mockup `select,input[type=text]` — 26px, elev, border, radius 4, padding 3×6. */
const FIELD = 'h-[26px] rounded border border-border bg-surface-elevated px-1.5 py-[3px] text-[12px] text-content placeholder:text-content-faint';
export const Select = forwardRef<HTMLSelectElement, SelectHTMLAttributes<HTMLSelectElement>>(function Select({ className = '', ...rest }, ref) {
  return <select ref={ref} className={`${FIELD} ${className}`} {...rest} />;
});
export const TextInput = forwardRef<HTMLInputElement, InputHTMLAttributes<HTMLInputElement>>(function TextInput({ className = '', type = 'text', ...rest }, ref) {
  return <input ref={ref} type={type} className={`${FIELD} ${className}`} {...rest} />;
});
