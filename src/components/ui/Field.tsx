import { forwardRef, type InputHTMLAttributes, type SelectHTMLAttributes, type TextareaHTMLAttributes } from 'react';

/** Mockup `select,input[type=text]` — 26px, elev, border, radius 4, padding 3×6. */
const FIELD = 'h-[26px] rounded border border-border bg-surface-elevated px-1.5 py-[3px] text-[12px] text-content placeholder:text-content-faint';
export const Select = forwardRef<HTMLSelectElement, SelectHTMLAttributes<HTMLSelectElement>>(function Select({ className = '', ...rest }, ref) {
  return <select ref={ref} className={`${FIELD} ${className}`} {...rest} />;
});
export const TextInput = forwardRef<HTMLInputElement, InputHTMLAttributes<HTMLInputElement>>(function TextInput({ className = '', type = 'text', ...rest }, ref) {
  return <input ref={ref} type={type} className={`${FIELD} ${className}`} {...rest} />;
});
/** Multi-line sibling of `TextInput` — same field style, height follows `rows`. */
const AREA = 'rounded border border-border bg-surface-elevated px-1.5 py-[3px] text-[12px] text-content placeholder:text-content-faint';
export const TextArea = forwardRef<HTMLTextAreaElement, TextareaHTMLAttributes<HTMLTextAreaElement>>(function TextArea({ className = '', ...rest }, ref) {
  return <textarea ref={ref} className={`${AREA} ${className}`} {...rest} />;
});
