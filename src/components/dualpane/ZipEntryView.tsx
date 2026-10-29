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

type FrameRow = [label: string, value: string | number | null | undefined];

/** The dialog's rows for one entry. */
function dialogRows(f: ZipEntryHeader['frame']): FrameRow[] {
  return [
    ['Object', f.object],
    ['Type', f.imagetyp],
    ['Filter', f.filter],
    ['Exposure', f.exptime != null ? `${f.exptime} s` : null],
    ['Date', f.date_obs],
    ['Camera', f.instrume],
    ['Telescope', f.telescop],
    ['Gain / offset', f.gain != null || f.offset != null ? `${f.gain ?? '–'} / ${f.offset ?? '–'}` : null],
    ['Size', f.naxis1 != null && f.naxis2 != null ? `${f.naxis1} × ${f.naxis2}` : null],
  ];
}

/** Metadata-pane fields, labelled and ordered like the editor's: what the header gives, read-only. */
const PANE_FIELDS: Array<[label: string, read: (f: ZipEntryHeader['frame']) => string | number | null | undefined]> = [
  ['OBJECT', (f) => f.object],
  ['FILTER', (f) => f.filter],
  ['IMAGETYP', (f) => f.imagetyp],
  ['CAMERA (INSTRUME)', (f) => f.instrume],
  ['TELESCOP', (f) => f.telescop],
  ['FOCALLEN (mm)', (f) => f.focallen],
  ['GAIN', (f) => f.gain],
  ['OFFSET', (f) => f.offset],
  ['XPIXSZ (µm)', (f) => f.xpixsz],
  ['BINNING', (f) => f.binning],
  ['EXPTIME (s)', (f) => f.exptime],
  ['CCD-TEMP (°C)', (f) => f.ccd_temp],
  ['DATE-OBS (ISO)', (f) => f.date_obs],
  ['RA', (f) => f.ra],
  ['DEC', (f) => f.dec],
  ['SIZE', (f) => (f.naxis1 != null && f.naxis2 != null ? `${f.naxis1} × ${f.naxis2}` : null)],
];

/** How many headers are read at once when a selection holds many entries. */
const HEADER_READS_AT_ONCE = 4;

type HeaderResult = { header: ZipEntryHeader } | { error: string };

/** The headers of several zip entries, read a few at a time while they are shown; a new `entries` list drops
 *  what was read. `results` fills as the reads finish. */
function useZipEntryHeaders(zipPath: string, entries: string[]): Map<string, HeaderResult> {
  const [results, setResults] = useState<Map<string, HeaderResult>>(new Map());
  const entriesKey = entries.join('\n');
  useEffect(() => {
    let cancelled = false;
    setResults(new Map());
    const queue = entriesKey ? entriesKey.split('\n') : [];
    const worker = async () => {
      for (let entry = queue.shift(); entry !== undefined; entry = queue.shift()) {
        let result: HeaderResult;
        try {
          result = { header: await api.invoke<ZipEntryHeader>('get_zip_entry_header', { zipPath, entry }) };
        } catch (err) {
          console.error('[ZipEntryView] get_zip_entry_header failed:', err);
          result = { error: String(err) };
        }
        if (cancelled) return;
        const done = entry;
        setResults((prev) => new Map(prev).set(done, result));
      }
    };
    for (let i = 0; i < HEADER_READS_AT_ONCE; i += 1) void worker();
    return () => {
      cancelled = true;
    };
  }, [zipPath, entriesKey]);
  return results;
}

/** The metadata pane's view of frame entries of a zip that the catalog holds no row for: what their headers
 *  say, in the editor's field order, read-only (there is no row to write to; a scan catalogs the entries). */
export function ZipEntriesMetadata({ zipPath, entries }: { zipPath: string; entries: string[] }) {
  const results = useZipEntryHeaders(zipPath, entries);
  const loaded = entries.flatMap((e) => {
    const r = results.get(e);
    return r && 'header' in r ? [r.header] : [];
  });
  const failed = entries.flatMap((e) => {
    const r = results.get(e);
    return r && 'error' in r ? [r.error] : [];
  });
  const pending = entries.length - loaded.length - failed.length;
  const rows = PANE_FIELDS.map(([label, read]) => {
    const values = new Set(loaded.map((h) => {
      const v = read(h.frame);
      return v == null || v === '' ? '' : String(v);
    }));
    return [label, values.size > 1 ? '(varies)' : [...values][0] ?? ''] as [string, string];
  }).filter(([, v]) => v !== '');
  const single = entries.length === 1 ? loaded[0] : undefined;

  return (
    <div className="space-y-2.5 p-4 text-sm" data-testid="zip-entries-metadata">
      <div className="text-content-muted" title={entries.length === 1 ? `${zipPath}/${entries[0]}` : zipPath}>
        {entries.length} {entries.length === 1 ? 'entry' : 'entries'} read from {getBasename(zipPath)}
        <span className="ml-2 text-xs">not in the catalog yet; a scan adds {entries.length === 1 ? 'it' : 'them'}</span>
      </div>
      {pending > 0 && <div className="text-xs text-content-muted">Reading the header{entries.length === 1 ? '' : 's'}… {loaded.length + failed.length} of {entries.length}</div>}
      {failed.length > 0 && (
        <div className="text-xs text-error" role="alert">
          {failed.length === 1 ? failed[0] : `${failed.length} headers could not be read: ${failed[0]}`}
        </div>
      )}
      {loaded.length > 0 && (rows.length > 0
        ? <KV items={rows} />
        : <div className="text-xs text-content-muted">The header names no object, filter or exposure.</div>)}
      {single && (
        <details>
          <summary className="cursor-pointer text-xs text-content-muted">Header</summary>
          <pre className="mt-1.5 max-h-[220px] overflow-auto rounded bg-surface-elevated p-2 text-[11px]">{single.header}</pre>
        </details>
      )}
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

  const rows = header ? dialogRows(header.frame) : [];

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
