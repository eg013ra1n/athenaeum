import { AlertTriangle } from 'lucide-react';
import type { RelinkResult } from '../../types/models';

/** Zips of the relinked folder that are not at the new location: their frames stay at the old path and show as
 *  missing. Nothing is deleted. Renders nothing when every zip was found. */
export function RelinkZipsNote({ result }: { result: RelinkResult }) {
  const n = result.zips_missing;
  if (!n) return null;
  return (
    <p role="status" className="mt-3 flex items-start gap-2 text-xs text-warning">
      <AlertTriangle size={14} className="mt-px flex-shrink-0" />
      <span>
        {n} zip{n === 1 ? '' : 's'} not found at the new location. Their frames show as missing and come back when the
        zip{n === 1 ? ' is' : 's are'} there again. A folder whose zips are gone for good can only be removed (Remove folder).
      </span>
    </p>
  );
}
