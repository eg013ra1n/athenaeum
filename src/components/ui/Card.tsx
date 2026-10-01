import type { ReactNode } from 'react';

/** Mockup `.card` + `.card h2` + `.sub`. The header row holds the heading,
 *  then the subtitle and the action beside it, so the heading's accessible
 *  name is the title alone. */
export function Card({ title, subtitle, action, children, className = '' }: { title?: ReactNode; subtitle?: ReactNode; action?: ReactNode; children?: ReactNode; className?: string }) {
  const hasTitle = title !== undefined && title !== null;
  const hasSubtitle = subtitle !== undefined && subtitle !== null;
  const hasAction = action !== undefined && action !== null;
  return (
    <section className={`rounded-lg border border-line bg-surface-elevated px-4 py-3.5 ${className}`}>
      {(hasTitle || hasSubtitle || hasAction) && (
        <div className="mb-2.5 flex items-center gap-2">
          {hasTitle && <h2 className="text-[13px] font-semibold text-content">{title}</h2>}
          {hasSubtitle && <span className="text-[12px] font-normal text-content-faint">{subtitle}</span>}
          {hasAction && <span className="ml-auto text-[12px] font-normal text-content">{action}</span>}
        </div>
      )}
      {children}
    </section>
  );
}
