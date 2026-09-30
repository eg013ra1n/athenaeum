import { useId, useLayoutEffect, useRef, type ReactNode } from 'react';
import { createPortal } from 'react-dom';
import { X } from 'lucide-react';
import { Button } from './Button';
import { isTopOverlay, popOverlay, pushOverlay } from './overlayStack';

const FOCUSABLE = 'button:not([disabled]),[href],input:not([disabled]),select:not([disabled]),textarea:not([disabled]),[tabindex]:not([tabindex="-1"])';

/** Spec §7 — the one modal shell: scrim, centred window, header, body, footer. */
export function DialogShell({ title, size = 'sm', onClose, busy = false, footer, children }: {
  title: ReactNode;
  size?: 'sm' | 'md';
  onClose: () => void;
  busy?: boolean;
  footer?: ReactNode;
  children: ReactNode;
}) {
  const titleId = useId();
  const ref = useRef<HTMLDivElement>(null);
  const busyRef = useRef(busy);
  busyRef.current = busy;
  const closeRef = useRef(onClose);
  closeRef.current = onClose;

  const pressOnScrim = useRef(false);

  useLayoutEffect(() => {
    const overlayId = pushOverlay();
    const opener = document.activeElement as HTMLElement | null;
    const first = ref.current?.querySelector<HTMLElement>('[data-autofocus]') ?? ref.current?.querySelector<HTMLElement>(FOCUSABLE);
    (first ?? ref.current)?.focus();
    const onKey = (e: KeyboardEvent) => {
      if (!isTopOverlay(overlayId)) return;
      if (e.key === 'Escape') {
        if (!busyRef.current) closeRef.current();
        return;
      }
      if (e.key !== 'Tab' || !ref.current) return;
      const items = [...ref.current.querySelectorAll<HTMLElement>(FOCUSABLE)];
      if (items.length === 0) return;
      const i = items.indexOf(document.activeElement as HTMLElement);
      if (!e.shiftKey && (i === items.length - 1 || i === -1)) { e.preventDefault(); items[0].focus(); }
      else if (e.shiftKey && i <= 0) { e.preventDefault(); items[items.length - 1].focus(); }
    };
    document.addEventListener('keydown', onKey);
    return () => {
      document.removeEventListener('keydown', onKey);
      popOverlay(overlayId);
      opener?.focus?.();
    };
  }, []);

  return createPortal(
    <div
      data-testid="dialog-scrim"
      className="fixed inset-0 z-50 flex items-center justify-center bg-[rgba(46,52,64,0.6)]"
      onMouseDown={(e) => { pressOnScrim.current = e.target === e.currentTarget; }}
      onClick={(e) => {
        const fromScrim = pressOnScrim.current && e.target === e.currentTarget;
        pressOnScrim.current = false;
        if (fromScrim && !busyRef.current) onClose();
      }}
    >
      <div
        ref={ref}
        role="dialog"
        aria-modal="true"
        aria-labelledby={titleId}
        tabIndex={-1}
        onClick={(e) => e.stopPropagation()}
        className={`max-w-[92vw] rounded-lg border border-border bg-surface-elevated px-4 py-3.5 text-[12.5px] leading-[1.4] text-content-muted shadow-[0_8px_24px_rgba(0,0,0,0.35)] ${size === 'md' ? 'w-[560px]' : 'w-[440px]'}`}
      >
        <div className="mb-2.5 flex items-start gap-2">
          <h2 id={titleId} className="min-w-0 flex-1 text-[13px] font-semibold text-content">{title}</h2>
          <Button size="sm" aria-label="Close" onClick={onClose} disabled={busy}><X size={12} /></Button>
        </div>
        <div>{children}</div>
        {footer !== undefined && <div className="mt-3.5 flex justify-end gap-2">{footer}</div>}
      </div>
    </div>,
    document.body,
  );
}
