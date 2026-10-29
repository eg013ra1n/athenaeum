// F8 on a zip row: the confirmation before a whole zip goes to the Black Hole.
//
// A zip is one file. Its frames stay in the catalog but are excluded from use (stacking, export, masters,
// duplicates, the sky chart) while the zip sits in the Black Hole; Restore on the Black Hole page brings it
// back. A zip that Move and ZIP wrote holds the only copy of its frames, so the dialog says so.

import { AlertTriangle } from 'lucide-react';
import { Button } from '../ui';
import { DialogShell } from '../ui/DialogShell';
import type { ZipListItem } from '../../types/archive';
import { formatBytes, getBasename } from './types';
import { zipFramesLabel, zipRemovalSummary } from './zipView';

const MAX_LISTED = 5;

export function ZipBlackHoleDialog({
  zips,
  fileCount,
  skippedDirs,
  onCancel,
  onConfirm,
}: {
  zips: ZipListItem[];
  /** Loose files selected together with the zips; they go to the Black Hole in the same step. */
  fileCount: number;
  skippedDirs: number;
  onCancel: () => void;
  onConfirm: () => void;
}) {
  const { frames, onlyCopies } = zipRemovalSummary(zips);
  const ours = zips.filter((z) => z.createdByOperation != null);
  const listed = zips.slice(0, MAX_LISTED);
  const title = zips.length === 1
    ? `Move ${getBasename(zips[0].path)} to the Black Hole?`
    : `Move ${zips.length} zips to the Black Hole?`;
  return (
    <DialogShell
      title={title}
      onClose={onCancel}
      footer={
        <>
          <Button data-autofocus onClick={onCancel}>Cancel</Button>
          <Button variant="dangerPrimary" onClick={onConfirm}>Move to Black Hole</Button>
        </>
      }
    >
      <ul className="mb-2.5 space-y-1" aria-label="Zips to move">
        {listed.map((z) => (
          <li key={z.path} className="flex items-baseline gap-2">
            <span className="min-w-0 flex-1 truncate font-mono text-content" title={z.path}>{getBasename(z.path)}</span>
            <span className="whitespace-nowrap">{zipFramesLabel(z.frameEntries)}</span>
            <span className="whitespace-nowrap">{formatBytes(z.size)}</span>
          </li>
        ))}
        {zips.length > listed.length && <li>and {zips.length - listed.length} more</li>}
      </ul>
      <p className="mb-2.5">
        The {frames.toLocaleString()} frame{frames === 1 ? '' : 's'} inside stay in the catalog but are left out of
        stacking, export, masters, duplicates and the sky chart until you restore the zip from the Black Hole page.
      </p>
      {onlyCopies > 0 && (
        <div
          role="alert"
          className="mb-2.5 flex items-start gap-2 rounded border border-warning/40 bg-warning-muted px-2.5 py-2 text-warning"
        >
          <AlertTriangle size={14} className="mt-px flex-shrink-0" />
          <span>
            This is the only copy of an archived frame set
            {ours.length < zips.length || ours.length > 1 ? ` (${ours.map((z) => getBasename(z.path)).join(', ')})` : ''}.
            Sending it to Void later deletes the frames for good.
          </span>
        </div>
      )}
      {fileCount > 0 && <p className="mb-2.5">{fileCount} selected file{fileCount === 1 ? '' : 's'} will be moved with it.</p>}
      {skippedDirs > 0 && (
        <p className="text-warning">
          {skippedDirs} selected folder{skippedDirs === 1 ? ' is' : 's are'} skipped: the Black Hole holds files and zips only.
        </p>
      )}
    </DialogShell>
  );
}
