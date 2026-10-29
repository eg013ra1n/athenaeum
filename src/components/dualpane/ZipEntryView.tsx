// A frame entry of a zip that the catalog holds no row for (a released entry whose unpacked copy is gone, one
// the scan skipped, one not scanned yet) is read straight from the zip: its metadata from the entry's first
// bytes (`get_zip_entry_header`) and its preview from a cache extraction (`preview_zip_entry`). Neither writes
// to the catalog; a scan catalogs the entry later.

import { useEffect, useState } from 'react';
import { api } from '../../api';
import { Button } from '../ui';
import { DialogShell } from '../ui/DialogShell';
import { KV } from '../ui/KV';
import type { ZipEntryHeader } from '../../types/archive';
import { getBasename } from './types';

type Bytes = Uint8Array<ArrayBuffer> | ArrayBuffer | number[];

/** Desktop answers with an `ArrayBuffer`, web with a `Uint8Array`; both are JPEG bytes. */
function toJpegBytes(payload: Bytes): Uint8Array<ArrayBuffer> {
  if (payload instanceof Uint8Array) return payload;
  if (payload instanceof ArrayBuffer) return new Uint8Array(payload);
  return new Uint8Array(payload);
}

/** The header of one zip entry, read when it is shown. `error` is the message to show, `header` null until loaded. */
function useZipEntryHeader(zipPath: string, entry: string): { header: ZipEntryHeader | null; error: string | null } {
  const [state, setState] = useState<{ header: ZipEntryHeader | null; error: string | null }>({ header: null, error: null });
  useEffect(() => {
    let cancelled = false;
    setState({ header: null, error: null });
    api
      .invoke<ZipEntryHeader>('get_zip_entry_header', { zipPath, entry })
      .then((header) => {
        if (!cancelled) setState({ header, error: null });
      })
      .catch((err) => {
        console.error('[ZipEntryView] get_zip_entry_header failed:', err);
        if (!cancelled) setState({ header: null, error: String(err) });
      });
    return () => {
      cancelled = true;
    };
  }, [zipPath, entry]);
  return state;
}

function summaryParts(h: ZipEntryHeader): string[] {
  const f = h.frame;
  return [
    f.object,
    f.filter,
    f.imagetyp,
    f.exptime != null ? `${f.exptime} s` : null,
    f.date_obs ? f.date_obs.slice(0, 10) : null,
  ].filter((p): p is string => !!p);
}

/** One muted line under a selected entry row: what the header says. */
export function ZipEntrySummary({ zipPath, entry }: { zipPath: string; entry: string }) {
  const { header, error } = useZipEntryHeader(zipPath, entry);
  if (error) return <div className="px-8 pb-1 text-xs text-error" role="alert">{error}</div>;
  if (!header) return <div className="px-8 pb-1 text-xs text-content-muted">Reading the header…</div>;
  const parts = summaryParts(header);
  return (
    <div className="px-8 pb-1 text-xs text-content-muted" data-testid="zip-entry-summary">
      {parts.length > 0 ? parts.join(' · ') : 'The header names no object, filter or exposure.'}
    </div>
  );
}

/** Metadata and preview of one zip entry. */
export function ZipEntryDialog({ zipPath, entry, onClose }: { zipPath: string; entry: string; onClose: () => void }) {
  const { header, error } = useZipEntryHeader(zipPath, entry);
  const [image, setImage] = useState<{ url: string | null; error: string | null }>({ url: null, error: null });

  useEffect(() => {
    let cancelled = false;
    let url: string | null = null;
    setImage({ url: null, error: null });
    api
      .invoke<Bytes>('preview_zip_entry', { zipPath, entry, resolution: 'preview' })
      .then((payload) => {
        const next = URL.createObjectURL(new Blob([toJpegBytes(payload)], { type: 'image/jpeg' }));
        if (cancelled) URL.revokeObjectURL(next);
        else {
          url = next;
          setImage({ url: next, error: null });
        }
      })
      .catch((err) => {
        console.error('[ZipEntryView] preview_zip_entry failed:', err);
        if (!cancelled) setImage({ url: null, error: String(err) });
      });
    return () => {
      cancelled = true;
      if (url) URL.revokeObjectURL(url);
    };
  }, [zipPath, entry]);

  const f = header?.frame;
  const rows: [string, string | number | null | undefined][] = f
    ? [
        ['Object', f.object],
        ['Type', f.imagetyp],
        ['Filter', f.filter],
        ['Exposure', f.exptime != null ? `${f.exptime} s` : null],
        ['Date', f.date_obs],
        ['Camera', f.instrume],
        ['Telescope', f.telescop],
        ['Gain / offset', f.gain != null || f.offset != null ? `${f.gain ?? '–'} / ${f.offset ?? '–'}` : null],
        ['Size', f.naxis1 != null && f.naxis2 != null ? `${f.naxis1} × ${f.naxis2}` : null],
      ]
    : [];

  return (
    <DialogShell
      title={<span className="font-mono">{getBasename(entry)}</span>}
      size="md"
      onClose={onClose}
      footer={<Button data-autofocus onClick={onClose}>Close</Button>}
    >
      <p className="mb-2.5 text-xs text-content-muted" title={`${zipPath}/${entry}`}>
        Read from {getBasename(zipPath)}. The catalog has no row for this entry yet; a scan adds it.
      </p>
      <div className="mb-2.5 flex min-h-[120px] items-center justify-center rounded border border-border bg-surface-elevated">
        {image.url ? (
          <img src={image.url} alt={`Preview of ${getBasename(entry)}`} className="max-h-[320px] max-w-full object-contain" />
        ) : image.error ? (
          <span className="p-3 text-xs text-error" role="alert">{image.error}</span>
        ) : (
          <span className="p-3 text-xs text-content-muted">Rendering the preview…</span>
        )}
      </div>
      {error ? (
        <div className="text-xs text-error" role="alert">{error}</div>
      ) : !header ? (
        <div className="text-xs text-content-muted">Reading the header…</div>
      ) : (
        <>
          <KV items={rows.filter(([, v]) => v != null && v !== '').map(([k, v]) => [k, String(v)] as [string, string])} />
          <details className="mt-2.5">
            <summary className="cursor-pointer text-xs text-content-muted">Header</summary>
            <pre className="mt-1.5 max-h-[220px] overflow-auto rounded bg-surface-elevated p-2 text-[11px]">{header.header}</pre>
          </details>
        </>
      )}
    </DialogShell>
  );
}
