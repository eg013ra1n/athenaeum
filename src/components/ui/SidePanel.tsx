import { useEffect, useLayoutEffect, useRef, useState, type ReactNode } from 'react';
import { X } from 'lucide-react';
import { Button } from './Button';

/** Spec §6.1 — a tab body that grows a 400px docked panel column while `panel` is set. */
export function PanelLayout({ panel, children }: { panel: ReactNode | null; children: ReactNode }) {
  return (
    <div className={panel ? 'grid grid-cols-[minmax(0,1fr)_400px] items-start gap-3.5' : ''}>
      <div className="min-w-0">{children}</div>
      {panel}
    </div>
  );
}

/** Spec §6.1 — sticky, own scroll, height = the viewport below its top edge. */
export function SidePanel({ title, label, onClose, children }: { title: ReactNode; label: string; onClose: () => void; children: ReactNode }) {
  const ref = useRef<HTMLElement>(null);
  const [height, setHeight] = useState<number | undefined>(undefined);
  const closeRef = useRef(onClose);
  closeRef.current = onClose;
  useLayoutEffect(() => {
    const fit = () => {
      if (!ref.current) return;
      const top = Math.max(0, ref.current.getBoundingClientRect().top);
      setHeight(Math.max(240, window.innerHeight - top - 16));
    };
    fit();
    window.addEventListener('resize', fit);
    return () => window.removeEventListener('resize', fit);
  }, []);
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== 'Escape' || e.defaultPrevented) return;
      if (document.querySelector('[role="dialog"]')) return; // an open dialog owns Escape
      closeRef.current();
    };
    document.addEventListener('keydown', onKey);
    return () => document.removeEventListener('keydown', onKey);
  }, []);
  return (
    <aside
      ref={ref}
      aria-label={label}
      style={height ? { height } : undefined}
      className="sticky top-0 overflow-auto rounded-lg border border-line bg-surface-elevated px-[18px] pb-[30px] pt-4 text-[12px] leading-[1.4]"
    >
      <div className="flex items-start gap-2">
        <div className="min-w-0 flex-1">{title}</div>
        <Button size="sm" aria-label="Close" onClick={onClose}><X size={12} /></Button>
      </div>
      {children}
    </aside>
  );
}
