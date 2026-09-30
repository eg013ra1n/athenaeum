import { Fragment, type ReactNode } from 'react';

/** Mockup `.kv` — grid auto/1fr, gap 3/14, 12px, faint terms. */
export function KV({ items }: { items: [ReactNode, ReactNode][] }) {
  return (
    <dl className="grid grid-cols-[auto_1fr] gap-x-3.5 gap-y-[3px] text-[12px]">
      {items.map(([k, v], i) => (
        <Fragment key={i}>
          <dt className="text-content-faint">{k}</dt>
          <dd className="m-0 min-w-0 text-content-secondary">{v}</dd>
        </Fragment>
      ))}
    </dl>
  );
}
