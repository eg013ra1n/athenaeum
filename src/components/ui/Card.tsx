import type { ReactNode } from 'react';

/** Mockup `.card` + `.card h2` + `.sub`. */
export function Card({ title, subtitle, action, children, className = '' }: { title?: ReactNode; subtitle?: ReactNode; action?: ReactNode; children?: ReactNode; className?: string }) {
  return (
    <section className={`rounded-lg border border-line bg-surface-elevated px-4 py-3.5 ${className}`}>
      {title !== undefined && (
        <h2 className="mb-2.5 flex items-center gap-2 text-[13px] font-semibold text-content">
          <span>{title}</span>
          {subtitle !== undefined && <>{' '}<span className="text-[12px] font-normal text-content-faint">{subtitle}</span></>}
          {action !== undefined && <>{' '}<span className="ml-auto text-[12px] font-normal">{action}</span></>}
        </h2>
      )}
      {children}
    </section>
  );
}
