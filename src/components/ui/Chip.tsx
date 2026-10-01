import type { ReactNode } from 'react';

/** Mockup `.chip` + `.c-ok/.c-warn/.c-err/.c-info/.c-mute` — 11px, line 18px, radius 3. */
export type ChipTone = 'ok' | 'warn' | 'err' | 'info' | 'mute' | 'pur';
const TONE: Record<ChipTone, string> = {
  ok: 'bg-success-muted text-success',
  warn: 'bg-warning-muted text-warning',
  err: 'bg-error-muted text-error',
  info: 'bg-info-muted text-info',
  mute: 'bg-surface-hover text-content-muted',
  pur: 'bg-purple/15 text-purple',
};
export function Chip({ tone, children, title, className = '' }: { tone: ChipTone; children: ReactNode; title?: string; className?: string }) {
  return (
    <span title={title} className={`inline-flex items-center gap-1 whitespace-nowrap rounded-[3px] px-1.5 text-[11px] leading-[18px] ${TONE[tone]} ${className}`}>
      {children}
    </span>
  );
}
