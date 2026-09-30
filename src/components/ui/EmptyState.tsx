import type { ReactNode } from 'react';

/** Mockup `.empty`. */
export function EmptyState({ children }: { children: ReactNode }) {
  return <p className="py-2.5 text-[12.5px] text-content-faint">{children}</p>;
}
