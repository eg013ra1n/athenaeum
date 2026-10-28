import { useEffect, useMemo, useState } from 'react';
import { api } from '../api';
import type { ExtractionEstimate } from '../types/archive';

/** The batch whose zip extraction is estimated: file ids, frame ids and/or a whole frame set (its lights
 *  plus the calibration frames it reaches). */
export interface ZipExtractionScope {
  fileIds?: number[];
  frameIds?: number[];
  frameSetId?: number | null;
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
  const empty = (fileIds?.length ?? 0) === 0 && (frameIds?.length ?? 0) === 0 && frameSetId == null;
  const key = useMemo(
    () => JSON.stringify([fileIds ?? [], frameIds ?? [], frameSetId]),
    [fileIds, frameIds, frameSetId],
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
        })
        .then((e) => { if (!cancelled) setEstimate(e); })
        .catch((err) => {
          console.error('[zip] estimate_zip_extraction failed:', err);
          if (!cancelled) setEstimate(null);
        });
    }, 250);
    return () => { cancelled = true; window.clearTimeout(timer); };
    // `key` stands for the three scope fields; listing them would re-run on every new array identity.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [key, empty, refreshKey]);

  return empty ? null : estimate;
}
