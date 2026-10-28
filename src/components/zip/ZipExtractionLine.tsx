import { HardDriveDownload } from 'lucide-react';
import { useZipExtractionEstimate, type ZipExtractionScope } from '../../hooks/useZipExtractionEstimate';
import { extractionLine } from '../../utils/zipChips';

/** "From zip: 120 files, 6.2 GB" — what a batch would extract from zips before it can read the frames.
 *  Renders nothing when no frame of the batch needs extracting. */
export function ZipExtractionLine({ scope, refreshKey, className = '' }: {
  scope: ZipExtractionScope | null;
  refreshKey?: number;
  className?: string;
}) {
  const estimate = useZipExtractionEstimate(scope, refreshKey);
  const line = extractionLine(estimate);
  if (!line) return null;
  return (
    <p
      data-testid="zip-extraction-line"
      title="These frames are inside zip files. They are extracted to the zip cache first, which takes disk space and time."
      className={`inline-flex items-center gap-1.5 text-xs text-info ${className}`}
    >
      <HardDriveDownload size={12} aria-hidden="true" />
      {line}
    </p>
  );
}
