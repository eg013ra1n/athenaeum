import { useEffect, useMemo, useState } from 'react';
import { api } from '../api';
import type { ExtractionEstimate } from '../types/archive';
import type { ExportMode } from '../types/export';

/** The batch whose zip extraction is estimated: file ids, frame ids and/or a whole frame set (its lights
 *  plus the calibration frames it reaches). */
export interface ZipExtractionScope {
  fileIds?: number[];
  frameIds?: number[];
  frameSetId?: number | null;
  /** With `frameSetId`: count only what this export mode reads (lights, plus the raw calibration originals
   *  or the masters, or nothing more). Without it the set counts every calibration file it can reach. */
  exportMode?: ExportMode;
  /** Also count the masters the named frames' calibration reads (a stacking run extracts them). */
  withLinkedMasters?: boolean;
}

/** What `scope` would extract from zips (rows not in the cache yet), for the "From zip: N files, X GB" line
 *  of a batch dialog. `null` while loading, when the scope is empty, or when the estimate failed: the line is
 *  informational, so a failure is logged and the dialog shows nothing. Re-asked 250 ms after the scope
 *  changes; `refreshKey` forces a fresh ask (e.g. after a run filled the cache). */
export function useZipExtractionEstimate(scope: ZipExtractionScope | null, refreshKey = 0): ExtractionEstimate | null {
  const [estimate, setEstimate] = useState<ExtractionEstimate | null>(null);
  const fileIds = scope?.fileIds;
  const frameIds = scope?.frameIds;
  const frameSetId = scope?.frameSetId ?? null;
  const exportMode = scope?.exportMode ?? null;
  const withLinkedMasters = scope?.withLinkedMasters ?? false;
  const empty = (fileIds?.length ?? 0) === 0 && (frameIds?.length ?? 0) === 0 && frameSetId == null;
  const key = useMemo(
    () => JSON.stringify([fileIds ?? [], frameIds ?? [], frameSetId, exportMode, withLinkedMasters]),
    [fileIds, frameIds, frameSetId, exportMode, withLinkedMasters],
  );

  useEffect(() => {
    if (empty) {
      setEstimate(null);
      return;
    }
    let cancelled = false;
    const timer = window.setTimeout(() => {
      api
        .invoke<ExtractionEstimate>('estimate_zip_extraction', {
          fileIds: fileIds ?? [],
          frameIds: frameIds ?? [],
          frameSetId,
          exportMode,
          withLinkedMasters,
        })
        .then((e) => { if (!cancelled) setEstimate(e); })
        .catch((err) => {
          console.error('[zip] estimate_zip_extraction failed:', err);
          if (!cancelled) setEstimate(null);
        });
    }, 250);
    return () => { cancelled = true; window.clearTimeout(timer); };
    // `key` stands for the scope fields; listing them would re-run on every new array identity.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [key, empty, refreshKey]);

  return empty ? null : estimate;
}
