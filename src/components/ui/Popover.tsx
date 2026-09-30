import { useLayoutEffect, useRef, type ReactNode } from 'react';
import { isTopOverlay, popOverlay, pushOverlay } from './overlayStack';

/** Mockup `.pop` — absolutely positioned under its `relative` parent. */
export function Popover({ open, onClose, children, className = '' }: { open: boolean; onClose: () => void; children: ReactNode; className?: string }) {
  const ref = useRef<HTMLDivElement>(null);
  const closeRef = useRef(onClose);
  closeRef.current = onClose;
  useLayoutEffect(() => {
    if (!open) return undefined;
    const overlayId = pushOverlay();
    const onKey = (e: KeyboardEvent) => { if (e.key === 'Escape' && isTopOverlay(overlayId)) closeRef.current(); };
    const onDown = (e: MouseEvent) => {
      if (ref.current && !ref.current.parentElement?.contains(e.target as Node)) closeRef.current();
    };
    document.addEventListener('keydown', onKey);
    document.addEventListener('mousedown', onDown);
    return () => {
      document.removeEventListener('keydown', onKey);
      document.removeEventListener('mousedown', onDown);
      popOverlay(overlayId);
    };
  }, [open]);
  if (!open) return null;
  return (
    <div ref={ref} className={`absolute right-0 top-full z-20 mt-1 grid min-w-[170px] gap-1 rounded-md border border-border bg-surface-elevated px-2.5 py-2 shadow-[0_8px_24px_rgba(0,0,0,0.35)] ${className}`}>
      {children}
    </div>
  );
}
